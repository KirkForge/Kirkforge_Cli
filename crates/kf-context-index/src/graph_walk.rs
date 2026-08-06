//! Graph-walk retrieval over the existing import + call-graph edges.
//!
//! BFS from a starting symbol, walking both directions:
//! - **reverse** (`imported_by`, `called_by`) — who depends on this symbol.
//! - **forward** (`imports`, `calls`) — what does this symbol depend on.
//!
//! Returns `(Symbol, hop_distance)` deduplicated by `(file, name)`
//! with the minimum hop distance kept. Capped at `max_hops` (default 2).
//! Results are ranked by a score that combines hop distance and edge
//! weight: call edges (weight 1.0) rank higher than import edges
//! (weight 0.5), and same-file symbols get a +0.3 bonus.
//!
//! ponytail: BFS over import/call edges with edge weighting. The
//! upgrade path is a weighted walk that fuses hop distance, edge type,
//! and embedding similarity into a single ranker.

use crate::{CallEdge, ContextIndex, ImportEdge, Symbol};
use std::collections::{HashMap, HashSet};

/// Edge type weight: how strongly a given edge type ties two symbols.
/// Call edges (1.0) rank higher than import edges (0.5) — calling is a
/// stronger relationship than importing.
const CALL_WEIGHT: f32 = 1.0;
const IMPORT_WEIGHT: f32 = 0.5;
/// Same-file bonus added to a symbol's score: symbols in the same file
/// as the start symbol are more strongly related than cross-file ones.
const SAME_FILE_BONUS: f32 = 0.3;

/// Walk the import + call graph from `start`, returning visited
/// symbols with their hop distance from the start.
///
/// `max_hops` caps the traversal; 0 returns only the start symbol.
/// Results are sorted by score descending: `score = edge_weight / hop`
/// plus a `+0.3` bonus when the symbol is in the same file as `start`.
/// Call edges use weight 1.0; import edges use weight 0.5, so callees
/// rank above importers at the same hop distance.
pub fn graph_walk(start: &Symbol, index: &ContextIndex, max_hops: usize) -> Vec<(Symbol, usize)> {
    let symbols = index.symbols();
    let edges = index.edges();
    let call_edges = index.call_edges();

    let mut best: HashMap<SymbolKey, (usize, usize)> = HashMap::new();
    let mut best_weight: HashMap<SymbolKey, f32> = HashMap::new();
    let mut visited: HashSet<SymbolKey> = HashSet::new();
    let mut frontier: Vec<(usize, usize)> = Vec::new();

    let start_idx = find_symbol_idx(symbols, start).unwrap_or(usize::MAX);
    if start_idx == usize::MAX {
        return vec![(start.clone(), 0)];
    }
    let start_key = key_of(&start_idx, symbols);
    best.insert(start_key.clone(), (start_idx, 0));
    best_weight.insert(start_key.clone(), CALL_WEIGHT);
    frontier.push((start_idx, 0));
    visited.insert(start_key);

    while let Some((idx, hops)) = frontier.pop() {
        if hops >= max_hops {
            continue;
        }
        let sym = &symbols[idx];
        for (next_idx, weight) in weighted_neighbors(sym, symbols, edges, call_edges) {
            let k = key_of(&next_idx, symbols);
            let next_hops = hops + 1;
            let is_new = !visited.contains(&k);
            let is_closer = best
                .get(&k)
                .is_none_or(|(_, prev_hops)| next_hops < *prev_hops);
            if is_new || is_closer {
                visited.insert(k.clone());
                best.insert(k.clone(), (next_idx, next_hops));
                best_weight.insert(k.clone(), weight);
                frontier.push((next_idx, next_hops));
            }
        }
    }

    let start_file = &symbols[start_idx].file;
    let mut scored: Vec<(f32, Symbol, usize)> = best
        .into_iter()
        .map(|(k, (idx, hops))| {
            let weight = best_weight.get(&k).copied().unwrap_or(CALL_WEIGHT);
            let score = score_for(hops, weight, &symbols[idx].file, start_file);
            (score, symbols[idx].clone(), hops)
        })
        .collect();
    scored.sort_by(|a, b| {
        b.0.partial_cmp(&a.0)
            .unwrap_or(std::cmp::Ordering::Equal)
            .then_with(|| a.1.name.cmp(&b.1.name))
    });
    scored.into_iter().map(|(_, s, h)| (s, h)).collect()
}

/// Compute the ranking score for a reached symbol. The score is
/// `edge_weight / hop_distance` plus a same-file bonus so that closer,
/// stronger, and co-located symbols rank higher. Call edges (weight
/// 1.0) outrank import edges (weight 0.5) at the same hop.
fn score_for(
    hops: usize,
    weight: f32,
    file: &std::path::Path,
    start_file: &std::path::Path,
) -> f32 {
    let h = hops.max(1) as f32;
    let base = weight / h;
    if file == start_file {
        base + SAME_FILE_BONUS
    } else {
        base
    }
}

/// Identifier for deduplication: file path + symbol name. Two symbols
/// with the same name in different files are distinct nodes; two
/// symbols with the same (file, name) are the same node regardless of
/// kind.
#[derive(Debug, Clone, PartialEq, Eq, Hash)]
struct SymbolKey {
    file: std::path::PathBuf,
    name: String,
}

fn key_of(idx: &usize, symbols: &[Symbol]) -> SymbolKey {
    SymbolKey {
        file: symbols[*idx].file.clone(),
        name: symbols[*idx].name.clone(),
    }
}

fn find_symbol_idx(symbols: &[Symbol], target: &Symbol) -> Option<usize> {
    symbols
        .iter()
        .position(|s| s.file == target.file && s.name == target.name)
}

/// Collect all neighbor symbol indices for `sym` across both edge
/// types and both directions, tagged with the edge weight (call edges
/// weigh 1.0, import edges weigh 0.5). Duplicates across edge types are
/// fine — the caller deduplicates and keeps the minimum hop.
fn weighted_neighbors<'a>(
    sym: &'a Symbol,
    symbols: &'a [Symbol],
    edges: &'a [ImportEdge],
    call_edges: &'a [CallEdge],
) -> Vec<(usize, f32)> {
    let mut out = Vec::new();

    for n in forward_imports(sym, edges, symbols) {
        out.push((n, IMPORT_WEIGHT));
    }
    for n in reverse_imports(sym, edges, symbols) {
        out.push((n, IMPORT_WEIGHT));
    }
    for n in forward_calls(sym, call_edges, symbols) {
        out.push((n, CALL_WEIGHT));
    }
    for n in reverse_calls(sym, call_edges, symbols) {
        out.push((n, CALL_WEIGHT));
    }

    out
}

/// What does this symbol's file import? (forward import edge).
fn forward_imports(sym: &Symbol, edges: &[ImportEdge], symbols: &[Symbol]) -> Vec<usize> {
    let mut out = Vec::new();
    for edge in edges.iter().filter(|e| e.source_file == sym.file) {
        if let Some(rf) = edge.resolved_file.as_ref() {
            for (i, s) in symbols.iter().enumerate() {
                if s.file == *rf {
                    out.push(i);
                }
            }
        }
    }
    out
}

/// Who imports this symbol's file? (reverse import edge).
fn reverse_imports(sym: &Symbol, edges: &[ImportEdge], symbols: &[Symbol]) -> Vec<usize> {
    let mut out = Vec::new();
    for edge in edges
        .iter()
        .filter(|e| e.resolved_file.as_ref().is_some_and(|rf| rf == &sym.file))
    {
        for (i, s) in symbols.iter().enumerate() {
            if s.file == edge.source_file {
                out.push(i);
            }
        }
    }
    out
}

/// What does this symbol call? (forward call edge — match caller name).
fn forward_calls(sym: &Symbol, call_edges: &[CallEdge], symbols: &[Symbol]) -> Vec<usize> {
    let mut out = Vec::new();
    for edge in call_edges
        .iter()
        .filter(|e| e.caller_name == sym.name && e.caller_file == sym.file)
    {
        for (i, s) in symbols.iter().enumerate() {
            if s.name == edge.callee_name {
                out.push(i);
            }
        }
    }
    out
}

/// Who calls this symbol? (reverse call edge — match callee name).
fn reverse_calls(sym: &Symbol, call_edges: &[CallEdge], symbols: &[Symbol]) -> Vec<usize> {
    let mut out = Vec::new();
    for edge in call_edges.iter().filter(|e| e.callee_name == sym.name) {
        for (i, s) in symbols.iter().enumerate() {
            if s.name == edge.caller_name && s.file == edge.caller_file {
                out.push(i);
            }
        }
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::{Symbol, SymbolKind};
    use std::path::PathBuf;

    fn sym(name: &str, file: &str) -> Symbol {
        Symbol {
            name: name.to_string(),
            kind: SymbolKind::Function,
            file: PathBuf::from(file),
            line: 1,
            end_line: 1,
            doc: None,
        }
    }

    fn idx_of(symbols: &[Symbol], name: &str, file: &str) -> usize {
        symbols
            .iter()
            .position(|s| s.name == name && s.file == PathBuf::from(file))
            .unwrap()
    }

    #[test]
    fn symbol_with_no_edges_returns_only_itself() {
        let symbols = vec![sym("lonely", "src/a.rs")];
        let index = ContextIndex::from_symbols(symbols.clone());
        let start = &symbols[0];
        let walked = graph_walk(start, &index, 2);
        assert_eq!(walked.len(), 1);
        assert_eq!(walked[0].0.name, "lonely");
        assert_eq!(walked[0].1, 0);
    }

    #[test]
    fn symbol_reaches_importer_within_one_hop() {
        let symbols = vec![sym("auth", "src/auth.rs"), sym("run", "src/main.rs")];
        let edges = vec![ImportEdge {
            source_file: PathBuf::from("src/main.rs"),
            imported_symbol: "crate::auth".to_string(),
            resolved_file: Some(PathBuf::from("src/auth.rs")),
            line: 1,
        }];
        let index = ContextIndex::from_symbols_and_edges(symbols.clone(), edges);
        let start = &symbols[idx_of(&symbols, "auth", "src/auth.rs")];
        let walked = graph_walk(start, &index, 2);
        let names: Vec<(&str, usize)> = walked.iter().map(|(s, h)| (s.name.as_str(), *h)).collect();
        assert!(
            names.iter().any(|(n, h)| *n == "run" && *h == 1),
            "expected 'run' within 1 hop, got {names:?}"
        );
        assert!(
            names.iter().any(|(n, h)| *n == "auth" && *h == 0),
            "start symbol 'auth' should be hop 0, got {names:?}"
        );
    }

    #[test]
    fn symbol_reaches_callee_within_one_hop() {
        let symbols = vec![sym("login", "src/main.rs"), sym("auth", "src/auth.rs")];
        let call_edges = vec![CallEdge {
            caller_file: PathBuf::from("src/main.rs"),
            caller_name: "login".to_string(),
            caller_line: 2,
            callee_name: "auth".to_string(),
            callee_file: Some(PathBuf::from("src/auth.rs")),
        }];
        let index =
            ContextIndex::from_symbols_and_edges_and_calls(symbols.clone(), vec![], call_edges);
        let start = &symbols[idx_of(&symbols, "auth", "src/auth.rs")];
        let walked = graph_walk(start, &index, 2);
        let names: Vec<(&str, usize)> = walked.iter().map(|(s, h)| (s.name.as_str(), *h)).collect();
        assert!(
            names.iter().any(|(n, h)| *n == "login" && *h == 1),
            "expected caller 'login' within 1 hop, got {names:?}"
        );
    }

    #[test]
    fn max_hops_limits_traversal() {
        let symbols = vec![
            sym("a", "src/a.rs"),
            sym("b", "src/b.rs"),
            sym("c", "src/c.rs"),
        ];
        let edges = vec![
            ImportEdge {
                source_file: PathBuf::from("src/b.rs"),
                imported_symbol: "crate::a".to_string(),
                resolved_file: Some(PathBuf::from("src/a.rs")),
                line: 1,
            },
            ImportEdge {
                source_file: PathBuf::from("src/c.rs"),
                imported_symbol: "crate::b".to_string(),
                resolved_file: Some(PathBuf::from("src/b.rs")),
                line: 1,
            },
        ];
        let index = ContextIndex::from_symbols_and_edges(symbols.clone(), edges);

        let start = &symbols[idx_of(&symbols, "a", "src/a.rs")];

        let walked1 = graph_walk(start, &index, 1);
        let names1: Vec<&str> = walked1.iter().map(|(s, _)| s.name.as_str()).collect();
        assert!(names1.contains(&"a"));
        assert!(names1.contains(&"b"));
        assert!(
            !names1.contains(&"c"),
            "max_hops=1 should not reach c (2 hops away), got {names1:?}"
        );

        let walked2 = graph_walk(start, &index, 2);
        let names2: Vec<&str> = walked2.iter().map(|(s, _)| s.name.as_str()).collect();
        assert!(names2.contains(&"c"), "max_hops=2 should reach c");
    }

    #[test]
    fn dedup_keeps_minimum_hop_distance() {
        let symbols = vec![sym("a", "src/a.rs"), sym("b", "src/b.rs")];
        let edges = vec![
            ImportEdge {
                source_file: PathBuf::from("src/b.rs"),
                imported_symbol: "crate::a".to_string(),
                resolved_file: Some(PathBuf::from("src/a.rs")),
                line: 1,
            },
            ImportEdge {
                source_file: PathBuf::from("src/b.rs"),
                imported_symbol: "crate::a".to_string(),
                resolved_file: Some(PathBuf::from("src/a.rs")),
                line: 2,
            },
        ];
        let index = ContextIndex::from_symbols_and_edges(symbols.clone(), edges);
        let start = &symbols[idx_of(&symbols, "a", "src/a.rs")];
        let walked = graph_walk(start, &index, 2);
        let b_entries: Vec<usize> = walked
            .iter()
            .filter(|(s, _)| s.name == "b")
            .map(|(_, h)| *h)
            .collect();
        assert_eq!(b_entries, vec![1], "b should appear exactly once at hop 1");
    }

    #[test]
    fn missing_start_symbol_returns_only_start() {
        let symbols = vec![sym("a", "src/a.rs")];
        let index = ContextIndex::from_symbols(symbols);
        let ghost = sym("ghost", "src/ghost.rs");
        let walked = graph_walk(&ghost, &index, 2);
        assert_eq!(walked.len(), 1);
        assert_eq!(walked[0].0.name, "ghost");
        assert_eq!(walked[0].1, 0);
    }

    #[test]
    fn test_retrieve_finds_function_by_name() {
        let foo = sym("foo", "src/lib.rs");
        let bar = sym("bar", "src/lib.rs");
        let symbols = vec![foo.clone(), bar];
        let call_edges = vec![CallEdge {
            caller_file: PathBuf::from("src/lib.rs"),
            caller_name: "bar".to_string(),
            caller_line: 5,
            callee_name: "foo".to_string(),
            callee_file: Some(PathBuf::from("src/lib.rs")),
        }];
        let index = ContextIndex::from_symbols_and_edges_and_calls(symbols, vec![], call_edges);
        let results = index.retrieve_hybrid("foo", 10);
        assert!(
            results.iter().any(|r| r.symbol.name == "foo"),
            "exact-name query 'foo' should find foo via graph walk, got {:?}",
            results
                .iter()
                .map(|r| r.symbol.name.as_str())
                .collect::<Vec<_>>()
        );
        let foo_result = results.iter().find(|r| r.symbol.name == "foo").unwrap();
        assert_eq!(foo_result.symbol.file, PathBuf::from("src/lib.rs"));
    }

    #[test]
    fn test_retrieve_ranks_closer_symbols_higher() {
        let a = sym("a", "src/a.rs");
        let b = sym("b", "src/b.rs");
        let c = sym("c", "src/c.rs");
        let symbols = vec![a.clone(), b.clone(), c.clone()];
        let call_edges = vec![
            CallEdge {
                caller_file: PathBuf::from("src/a.rs"),
                caller_name: "a".to_string(),
                caller_line: 2,
                callee_name: "b".to_string(),
                callee_file: Some(PathBuf::from("src/b.rs")),
            },
            CallEdge {
                caller_file: PathBuf::from("src/b.rs"),
                caller_name: "b".to_string(),
                caller_line: 2,
                callee_name: "c".to_string(),
                callee_file: Some(PathBuf::from("src/c.rs")),
            },
        ];
        let index = ContextIndex::from_symbols_and_edges_and_calls(symbols, vec![], call_edges);
        let start = &a;
        let walked = graph_walk(start, &index, 2);
        let names: Vec<(&str, usize)> = walked.iter().map(|(s, h)| (s.name.as_str(), *h)).collect();
        assert!(
            names.iter().any(|(n, h)| *n == "a" && *h == 0),
            "start 'a' should be hop 0, got {names:?}"
        );
        assert!(
            names.iter().any(|(n, h)| *n == "b" && *h == 1),
            "'b' should be at hop 1, got {names:?}"
        );
        assert!(
            names.iter().any(|(n, h)| *n == "c" && *h == 2),
            "'c' should be at hop 2, got {names:?}"
        );
        let b_pos = walked.iter().position(|(s, _)| s.name == "b").unwrap();
        let c_pos = walked.iter().position(|(s, _)| s.name == "c").unwrap();
        assert!(
            b_pos < c_pos,
            "b (hop 1) should rank higher than c (hop 2), got order {:?}",
            walked
                .iter()
                .map(|(s, h)| (s.name.as_str(), *h))
                .collect::<Vec<_>>()
        );
    }

    #[test]
    fn call_edge_ranks_higher_than_import_edge_at_same_hop() {
        let start = sym("start", "src/main.rs");
        let callee = sym("callee", "src/lib.rs");
        let importer = sym("importer", "src/other.rs");
        let symbols = vec![start.clone(), callee.clone(), importer.clone()];
        let edges = vec![ImportEdge {
            source_file: PathBuf::from("src/other.rs"),
            imported_symbol: "crate::main".to_string(),
            resolved_file: Some(PathBuf::from("src/main.rs")),
            line: 1,
        }];
        let call_edges = vec![CallEdge {
            caller_file: PathBuf::from("src/main.rs"),
            caller_name: "start".to_string(),
            caller_line: 2,
            callee_name: "callee".to_string(),
            callee_file: Some(PathBuf::from("src/lib.rs")),
        }];
        let index = ContextIndex::from_symbols_and_edges_and_calls(symbols, edges, call_edges);
        let walked = graph_walk(&start, &index, 1);
        let callee_pos = walked.iter().position(|(s, _)| s.name == "callee");
        let importer_pos = walked.iter().position(|(s, _)| s.name == "importer");
        assert!(
            callee_pos.is_some(),
            "callee should be reached, got {walked:?}"
        );
        assert!(
            importer_pos.is_some(),
            "importer should be reached, got {walked:?}"
        );
        assert!(
            callee_pos.unwrap() < importer_pos.unwrap(),
            "call edge (callee) should rank higher than import edge (importer) at same hop, got {:?}",
            walked.iter().map(|(s, h)| (s.name.as_str(), *h)).collect::<Vec<_>>()
        );
    }

    #[test]
    fn same_file_symbol_ranks_higher_than_cross_file() {
        let start = sym("start", "src/main.rs");
        let same_file = sym("helper", "src/main.rs");
        let other_file = sym("external", "src/other.rs");
        let symbols = vec![start.clone(), same_file.clone(), other_file.clone()];
        let call_edges = vec![
            CallEdge {
                caller_file: PathBuf::from("src/main.rs"),
                caller_name: "start".to_string(),
                caller_line: 2,
                callee_name: "helper".to_string(),
                callee_file: Some(PathBuf::from("src/main.rs")),
            },
            CallEdge {
                caller_file: PathBuf::from("src/main.rs"),
                caller_name: "start".to_string(),
                caller_line: 3,
                callee_name: "external".to_string(),
                callee_file: Some(PathBuf::from("src/other.rs")),
            },
        ];
        let index = ContextIndex::from_symbols_and_edges_and_calls(symbols, vec![], call_edges);
        let walked = graph_walk(&start, &index, 1);
        let same_pos = walked.iter().position(|(s, _)| s.name == "helper").unwrap();
        let cross_pos = walked
            .iter()
            .position(|(s, _)| s.name == "external")
            .unwrap();
        assert!(
            same_pos < cross_pos,
            "same-file symbol should rank higher than cross-file at same hop, got {:?}",
            walked
                .iter()
                .map(|(s, h)| (s.name.as_str(), *h))
                .collect::<Vec<_>>()
        );
    }
}
