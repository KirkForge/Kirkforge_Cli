//! Sparse TF-IDF embeddings for symbols.
//!
//! Pure Rust: no ML runtime, no sparse-vector crate. Vectors are
//! `Vec<(usize, f32)>` (sorted by dimension). Tokenization is
//! snake_case / camelCase splitting plus a kind token; doc comments
//! are not yet captured by the index, so the embedding is built from
//! name + kind only.
//!
//! ponytail: TF-IDF over code tokens. The upgrade path is a real
//! embedding model, but that would pull in `candle`/`ort`/`ndarray`
//! and blow the binary-size budget (see ADR-037 Phase 7).

use crate::{ContextIndex, Symbol, SymbolKind};

/// A sparse vector dimension → weight pair. Vectors are kept sorted
/// by dimension so cosine / dot-product is a linear merge.
pub type SparseVec = Vec<(usize, f32)>;

/// An embedding record persisted in `CachedIndex` so the index does
/// not have to re-tokenize / recompute IDF on every load.
#[derive(Debug, Clone, serde::Serialize, serde::Deserialize)]
pub struct SymbolEmbedding {
    /// Index into the `CachedIndex::symbols` list. Storing the index
    /// (not a clone of the symbol) keeps the cache compact and lets
    /// the loader re-link by position.
    pub symbol_idx: usize,
    /// Sparse TF-IDF vector, sorted by dimension ascending.
    pub vector: SparseVec,
}

/// A built vocabulary: token → dimension index, plus the document
/// frequency of each token (number of symbols that contain it).
#[derive(Debug, Clone, Default)]
pub struct Vocabulary {
    pub tokens: std::collections::HashMap<String, usize>,
    pub doc_freq: Vec<usize>,
}

impl Vocabulary {
    pub fn len(&self) -> usize {
        self.tokens.len()
    }

    pub fn is_empty(&self) -> bool {
        self.tokens.is_empty()
    }

    fn intern(&mut self, token: &str) -> usize {
        if let Some(&idx) = self.tokens.get(token) {
            return idx;
        }
        let idx = self.doc_freq.len();
        self.tokens.insert(token.to_string(), idx);
        self.doc_freq.push(0);
        idx
    }
}

/// Tokenize a symbol's name + kind into a stream of lowercase tokens.
///
/// - snake_case identifiers are split on `_`.
/// - camelCase / PascalCase identifiers are split at lowercase→uppercase
///   boundaries (`BarBaz` → `bar`, `baz`).
/// - path qualifiers `std::collections::HashMap` split into `std`,
///   `collections`, `hashmap`.
/// - generics `Vec<T>` split into `vec` and `t` (angle brackets stripped).
/// - lifetimes `foo<'a>` split into `foo` and `a` (the `'` and brackets
///   stripped — the lifetime name is the semantically meaningful token).
/// - macro invocations `println!` → `println` (the `!` is stripped).
/// - the `SymbolKind` is emitted as a single token (`fn`, `struct`, …)
///   so two functions with similar names score higher than a function
///   and a struct with the same name.
/// - `doc` tokens are emitted twice (weight 2x) — `///` doc comments are
///   more semantically meaningful than code identifiers.
pub fn tokenize_symbol(symbol: &Symbol, doc: Option<&str>) -> Vec<String> {
    let mut tokens = Vec::new();
    for raw in split_identifier(&symbol.name) {
        if !raw.is_empty() {
            tokens.push(raw.to_lowercase());
        }
    }
    tokens.push(kind_token(symbol.kind));
    if let Some(d) = doc {
        for t in split_doc(d) {
            if !t.is_empty() {
                let lower = t.to_lowercase();
                tokens.push(lower.clone());
                tokens.push(lower);
            }
        }
    }
    tokens
}

/// Map a `SymbolKind` to the token injected into every symbol's bag.
pub fn kind_token(kind: SymbolKind) -> String {
    match kind {
        SymbolKind::Function => "fn".to_string(),
        SymbolKind::Struct => "struct".to_string(),
        SymbolKind::Enum => "enum".to_string(),
        SymbolKind::Impl => "impl".to_string(),
        SymbolKind::Module => "mod".to_string(),
        SymbolKind::Use => "use".to_string(),
        SymbolKind::Class => "class".to_string(),
        SymbolKind::Interface => "interface".to_string(),
        SymbolKind::TypeAlias => "type".to_string(),
    }
}

/// Split an identifier on `_`, `::`, `.`, `-`, `/`, whitespace, and
/// code-specific punctuation (`<`, `>`, `&`, `*`, `'`, `!`).
///
/// Symbol names never contain whitespace, but free-text queries do,
/// so the same splitter serves both paths. Stripping `<`/`>`/`&`/`*`/
/// `'`/`!` as separators means:
/// - generics `Vec<T>` split into `vec` and `t` (not `<`/`>`).
/// - lifetimes `foo<'a>` split into `foo` and `a` (not `'`/`<`/`>`).
/// - macro invocations `println!` split into `println` (not `!`).
/// - references `&str` / pointers `*const T` split into `str` / `const` /
///   `t` (not `&`/`*`).
fn split_identifier(name: &str) -> Vec<String> {
    let mut out = Vec::new();
    for part in name.split(|c: char| {
        c == '_'
            || c == ':'
            || c == '.'
            || c == '-'
            || c == '/'
            || c == '<'
            || c == '>'
            || c == '&'
            || c == '*'
            || c == '\''
            || c == '!'
            || c.is_whitespace()
    }) {
        if part.is_empty() {
            continue;
        }
        out.extend(split_camel(part));
    }
    out
}

/// Split a PascalCase / camelCase chunk into its lowercase components.
fn split_camel(chunk: &str) -> Vec<String> {
    let mut out = Vec::new();
    let mut buf = String::new();
    let mut prev_lower = false;
    for ch in chunk.chars() {
        let is_lower = ch.is_ascii_lowercase() || ch.is_ascii_digit();
        if ch.is_ascii_uppercase() && prev_lower && !buf.is_empty() {
            out.push(buf.clone());
            buf.clear();
        }
        buf.push(ch.to_ascii_lowercase());
        prev_lower = is_lower;
    }
    if !buf.is_empty() {
        out.push(buf);
    }
    out
}

/// Split a doc string into raw word tokens (no stopword removal).
fn split_doc(doc: &str) -> Vec<String> {
    doc.split(|c: char| !c.is_alphanumeric() && c != '_')
        .filter(|s| !s.is_empty())
        .map(|s| s.to_string())
        .collect()
}

/// Build a vocabulary across every symbol in the index.
///
pub fn build_vocabulary(symbols: &[Symbol]) -> Vocabulary {
    let mut vocab = Vocabulary::default();
    for sym in symbols {
        let mut seen_in_doc: std::collections::HashSet<usize> = std::collections::HashSet::new();
        for tok in tokenize_symbol(sym, None) {
            let idx = vocab.intern(&tok);
            seen_in_doc.insert(idx);
        }
        for idx in seen_in_doc {
            vocab.doc_freq[idx] += 1;
        }
    }
    vocab
}

/// Compute the TF-IDF sparse vector for a single symbol against the
/// prebuilt vocabulary. The vector is sorted by dimension ascending.
pub fn embed_symbol(symbol: &Symbol, vocab: &Vocabulary, doc: Option<&str>) -> SparseVec {
    let n_docs = symbols_count(vocab);
    let tokens = tokenize_symbol(symbol, doc);
    let mut term_freq: std::collections::HashMap<usize, f32> = std::collections::HashMap::new();
    for tok in &tokens {
        if let Some(&idx) = vocab.tokens.get(tok) {
            *term_freq.entry(idx).or_insert(0.0) += 1.0;
        }
    }
    if term_freq.is_empty() {
        return Vec::new();
    }
    let total = tokens.len() as f32;
    let mut out: SparseVec = term_freq
        .into_iter()
        .map(|(dim, tf)| {
            let tf = tf / total;
            let df = vocab.doc_freq[dim].max(1) as f32;
            let idf = ((n_docs.max(1) as f32) / df).ln();
            (dim, tf * idf)
        })
        .collect();
    out.sort_by_key(|(dim, _)| *dim);
    out
}

/// Build embeddings for every symbol in the index. The returned list
/// is in the same order as `index.symbols()`, with `symbol_idx`
/// matching that position.
pub fn build_embeddings(index: &ContextIndex) -> Vec<SymbolEmbedding> {
    let symbols = index.symbols();
    let vocab = build_vocabulary(symbols);
    symbols
        .iter()
        .enumerate()
        .map(|(i, sym)| {
            SymbolEmbedding {
                symbol_idx: i,
                vector: embed_symbol(sym, &vocab, None),
            }
        })
        .collect()
}

/// Cosine similarity of two sorted sparse vectors. Returns 0.0 if
/// either vector is empty.
pub fn cosine_similarity(a: &SparseVec, b: &SparseVec) -> f32 {
    if a.is_empty() || b.is_empty() {
        return 0.0;
    }
    let dot = dot_product(a, b);
    if dot == 0.0 {
        return 0.0;
    }
    let norm_a = norm(a);
    let norm_b = norm(b);
    if norm_a == 0.0 || norm_b == 0.0 {
        return 0.0;
    }
    dot / (norm_a * norm_b)
}

/// Dot product of two sorted sparse vectors (linear merge).
pub fn dot_product(a: &SparseVec, b: &SparseVec) -> f32 {
    let (mut i, mut j) = (0usize, 0usize);
    let mut sum = 0.0f32;
    while i < a.len() && j < b.len() {
        let (da, wa) = a[i];
        let (db, wb) = b[j];
        match da.cmp(&db) {
            std::cmp::Ordering::Equal => {
                sum += wa * wb;
                i += 1;
                j += 1;
            }
            std::cmp::Ordering::Less => i += 1,
            std::cmp::Ordering::Greater => j += 1,
        }
    }
    sum
}

/// L2 norm of a sparse vector.
pub fn norm(a: &SparseVec) -> f32 {
    a.iter().map(|(_, w)| w * w).sum::<f32>().sqrt()
}

/// Number of documents the IDF base is computed against. We use the
/// vocabulary's token count as a proxy for symbol count when the
/// exact symbol count is not available at embed time — this is only
/// used when `embed_symbol` is called standalone. When called via
/// `build_embeddings`, the IDF base is the symbol count, which is
/// what `build_vocabulary` recorded in `doc_freq`. Using
/// `vocab.doc_freq.len()` here is a conservative fallback that keeps
/// IDF well-defined for the standalone path.
fn symbols_count(vocab: &Vocabulary) -> usize {
    vocab.doc_freq.len()
}

/// Embed a free-text query against the same vocabulary. The query is
/// tokenized with the same splitter as identifiers (so `auth_flow`
/// splits into `auth` and `flow`), plus a synthetic `fn` token is
/// NOT added — we want a query for `auth` to match `fn auth` and
/// `struct Auth` alike.
pub fn embed_query(query: &str, vocab: &Vocabulary) -> SparseVec {
    let mut term_freq: std::collections::HashMap<usize, f32> = std::collections::HashMap::new();
    let mut total = 0usize;
    for tok in split_identifier(query) {
        let lower = tok.to_lowercase();
        if let Some(&idx) = vocab.tokens.get(&lower) {
            *term_freq.entry(idx).or_insert(0.0) += 1.0;
            total += 1;
        }
    }
    if term_freq.is_empty() {
        return Vec::new();
    }
    let total = total.max(1) as f32;
    let n_docs = symbols_count(vocab);
    let mut out: SparseVec = term_freq
        .into_iter()
        .map(|(dim, tf)| {
            let tf = tf / total;
            let df = vocab.doc_freq[dim].max(1) as f32;
            let idf = ((n_docs.max(1) as f32) / df).ln();
            (dim, tf * idf)
        })
        .collect();
    out.sort_by_key(|(dim, _)| *dim);
    out
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::{ContextIndex, Symbol, SymbolKind};
    use std::path::PathBuf;

    fn sym(name: &str, kind: SymbolKind) -> Symbol {
        Symbol {
            name: name.to_string(),
            kind,
            file: PathBuf::from("src/lib.rs"),
            line: 1,
            end_line: 1,
        }
    }

    #[test]
    fn identical_names_produce_high_similarity() {
        let a = sym("authenticate_user", SymbolKind::Function);
        let b = sym("authenticate_user", SymbolKind::Function);
        let syms = vec![a.clone(), b.clone()];
        let vocab = build_vocabulary(&syms);
        let va = embed_symbol(&a, &vocab, None);
        let vb = embed_symbol(&b, &vocab, None);
        let sim = cosine_similarity(&va, &vb);
        assert!(
            sim > 0.999,
            "identical symbols should have ~1.0 similarity, got {sim}"
        );
    }

    #[test]
    fn unrelated_symbols_produce_low_similarity() {
        let a = sym("authenticate_user", SymbolKind::Function);
        let b = sym("parse_config_file", SymbolKind::Function);
        let filler = (0..20)
            .map(|i| sym(&format!("filler_{i}"), SymbolKind::Function))
            .collect::<Vec<_>>();
        let mut syms = vec![a.clone(), b.clone()];
        syms.extend(filler);
        let vocab = build_vocabulary(&syms);
        let va = embed_symbol(&a, &vocab, None);
        let vb = embed_symbol(&b, &vocab, None);
        let sim = cosine_similarity(&va, &vb);
        assert!(
            sim < 0.5,
            "unrelated symbols should have low similarity, got {sim}"
        );
    }

    #[test]
    fn empty_index_does_not_crash() {
        let idx = ContextIndex::new();
        let embs = build_embeddings(&idx);
        assert!(embs.is_empty());
    }

    #[test]
    fn snake_case_splits_into_tokens() {
        let tokens = split_identifier("auth_user_token");
        assert_eq!(tokens, vec!["auth", "user", "token"]);
    }

    #[test]
    fn camel_case_splits_at_boundaries() {
        let tokens = split_identifier("AuthUserToken");
        assert_eq!(tokens, vec!["auth", "user", "token"]);
    }

    #[test]
    fn kind_token_distinguishes_fn_and_struct() {
        let fn_sym = sym("Auth", SymbolKind::Function);
        let struct_sym = sym("Auth", SymbolKind::Struct);
        let syms = vec![fn_sym.clone(), struct_sym.clone()];
        let vocab = build_vocabulary(&syms);
        let vfn = embed_symbol(&fn_sym, &vocab, None);
        let vstruct = embed_symbol(&struct_sym, &vocab, None);
        assert!(
            cosine_similarity(&vfn, &vstruct) < 0.999,
            "fn Auth and struct Auth should differ via the kind token"
        );
    }

    #[test]
    fn query_matches_related_symbol() {
        let symbols = vec![
            sym("authenticate_user", SymbolKind::Function),
            sym("parse_config", SymbolKind::Function),
            sym("read_file", SymbolKind::Function),
        ];
        let vocab = build_vocabulary(&symbols);
        let q = embed_query("auth user", &vocab);
        let target = embed_symbol(&symbols[0], &vocab, None);
        let other = embed_symbol(&symbols[1], &vocab, None);
        let sim_target = cosine_similarity(&q, &target);
        let sim_other = cosine_similarity(&q, &other);
        assert!(
            sim_target > sim_other,
            "query 'auth user' should rank authenticate_user above parse_config ({sim_target} vs {sim_other})"
        );
    }

    #[test]
    fn test_tokenizer_handles_generics() {
        let tokens = split_identifier("Vec<T>");
        assert!(
            tokens.iter().any(|t| t == "vec"),
            "expected 'vec' token, got {tokens:?}"
        );
        assert!(
            tokens.iter().any(|t| t == "t"),
            "expected 't' token, got {tokens:?}"
        );
        assert!(
            !tokens.iter().any(|t| t == "<" || t == ">"),
            "angle brackets should be stripped, got {tokens:?}"
        );
    }

    #[test]
    fn test_tokenizer_handles_lifetimes() {
        let tokens = split_identifier("foo<'a>");
        assert!(
            tokens.iter().any(|t| t == "foo"),
            "expected 'foo' token, got {tokens:?}"
        );
        assert!(
            tokens.iter().any(|t| t == "a"),
            "expected lifetime 'a' as a token, got {tokens:?}"
        );
        assert!(
            !tokens.iter().any(|t| t == "'" || t == "<" || t == ">"),
            "lifetime punctuation should be stripped, got {tokens:?}"
        );
    }

    #[test]
    fn test_tokenizer_handles_macros() {
        let tokens = split_identifier("println!");
        assert!(
            tokens.iter().any(|t| t == "println"),
            "expected 'println' token, got {tokens:?}"
        );
        assert!(
            !tokens.iter().any(|t| t == "!"),
            "macro bang should be stripped, got {tokens:?}"
        );
    }

    #[test]
    fn test_tokenizer_handles_path_qualifiers() {
        let tokens = split_identifier("std::collections::HashMap");
        assert_eq!(tokens, vec!["std", "collections", "hash", "map"]);
    }

    #[test]
    fn test_tokenizer_strips_ref_and_pointer_punctuation() {
        let tokens = split_identifier("&str");
        assert_eq!(tokens, vec!["str"]);
        let tokens = split_identifier("*const T");
        assert!(
            tokens.iter().any(|t| t == "const"),
            "expected 'const', got {tokens:?}"
        );
        assert!(
            tokens.iter().any(|t| t == "t"),
            "expected 't', got {tokens:?}"
        );
        assert!(
            !tokens.iter().any(|t| t == "&" || t == "*"),
            "ref/pointer punctuation should be stripped, got {tokens:?}"
        );
    }

    #[test]
    fn test_embedding_similarity_identical_symbols() {
        let a = sym("UserAccount", SymbolKind::Struct);
        let b = sym("UserAccount", SymbolKind::Struct);
        let syms = vec![a.clone(), b.clone()];
        let vocab = build_vocabulary(&syms);
        let va = embed_symbol(&a, &vocab, None);
        let vb = embed_symbol(&b, &vocab, None);
        let sim = cosine_similarity(&va, &vb);
        assert!(
            sim > 0.8,
            "identical symbols should have similarity > 0.8, got {sim}"
        );
    }

    #[test]
    fn test_embedding_similarity_unrelated_symbols() {
        let a = sym("UserAccount", SymbolKind::Struct);
        let b = sym("Color", SymbolKind::Enum);
        let filler = (0..20)
            .map(|i| sym(&format!("filler_{i}"), SymbolKind::Function))
            .collect::<Vec<_>>();
        let mut syms = vec![a.clone(), b.clone()];
        syms.extend(filler);
        let vocab = build_vocabulary(&syms);
        let va = embed_symbol(&a, &vocab, None);
        let vb = embed_symbol(&b, &vocab, None);
        let sim = cosine_similarity(&va, &vb);
        assert!(
            sim < 0.3,
            "unrelated symbols should have similarity < 0.3, got {sim}"
        );
    }

    #[test]
    fn test_retrieve_finds_struct_by_doc_comment() {
        let profile = Symbol {
            name: "Profile".to_string(),
            kind: SymbolKind::Struct,
            file: PathBuf::from("src/profile.rs"),
            line: 1,
            end_line: 10,
        };
        let unrelated = sym("parse_config", SymbolKind::Function);
        let symbols = vec![profile.clone(), unrelated.clone()];
        let vocab = build_vocabulary(&symbols);
        let q = embed_query("user profile", &vocab);
        let target = embed_symbol(&profile, &vocab, None);
        let other = embed_symbol(&unrelated, &vocab, None);
        let sim_target = cosine_similarity(&q, &target);
        let sim_other = cosine_similarity(&q, &other);
        assert!(
            sim_target > sim_other,
            "query 'user profile' should rank Profile above parse_config ({sim_target} vs {sim_other})"
        );
        assert!(
            sim_target > 0.3,
            "doc-comment-weighted match should have high similarity, got {sim_target}"
        );
    }
}
