//! Graph-walk retrieval over the existing import + call-graph edges.
//!
//! BFS from a starting symbol, walking both directions:
//! - **reverse** (`imported_by`, `called_by`) — who depends on this symbol.
//! - **forward** (`imports`, `calls`) — what does this symbol depend on.
//!
//! Returns `(Symbol, hop_distance)` deduplicated by `(file, name)`
//! with the minimum hop distance kept. Capped at `max_hops` (default 2).
//!
//! ponytail: BFS over import/call edges. The upgrade path is a
//! weighted walk (call frequency, import distance) but the unweighted
//! BFS is enough to pull in a symbol's immediate neighbourhood.

use crate::{CallEdge, ContextIndex, ImportEdge, Symbol};
use std::collections::{HashMap, HashSet};

/// Walk the import + call graph from `start`, returning visited
/// symbols with their hop distance from the start.
///
/// `max_hops` caps the traversal; 0 returns only the start symbol.
pub fn graph_walk(start: &Symbol, index: &ContextIndex, max_hops: usize) -> Vec<(Symbol, usize)> {
    let symbols = index.symbols();
    let edges = index.edges();
    let call_edges = index.call_edges();

    let mut best: HashMap<SymbolKey, (usize, usize)> = HashMap::new();
    let mut visited: HashSet<SymbolKey> = HashSet::new();
    let mut frontier: Vec<(usize, usize)> = Vec::new();

    let start_idx = find_symbol_idx(symbols, start).unwrap_or(usize::MAX);
    if start_idx == usize::MAX {
        return vec![(start.clone(), 0)];
    }
    best.insert(key_of(&start_idx, symbols), (start_idx, 0));
    frontier.push((start_idx, 0));
    visited.insert(key_of(&start_idx, symbols));

    while let Some((idx, hops)) = frontier.pop() {
        if hops >= max_hops {
            continue;
        }
        let sym = &symbols[idx];
        for next_idx in neighbors(sym, symbols, edges, call_edges) {
            let k = key_of(&next_idx, symbols);
            let next_hops = hops + 1;
            let is_new = !visited.contains(&k);
            let is_closer = best
                .get(&k)
                .is_none_or(|(_, prev_hops)| next_hops < *prev_hops);
            if is_new || is_closer {
                visited.insert(k.clone());
                best.insert(k.clone(), (next_idx, next_hops));
                frontier.push((next_idx, next_hops));
            }
        }
    }

    let mut out: Vec<(Symbol, usize)> = best
        .into_values()
        .map(|(idx, hops)| (symbols[idx].clone(), hops))
        .collect();
    out.sort_by_key(|(_, h)| *h);
    out
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
/// types and both directions. Duplicates across edge types are fine —
/// the caller deduplicates.
fn neighbors<'a>(
    sym: &'a Symbol,
    symbols: &'a [Symbol],
    edges: &'a [ImportEdge],
    call_edges: &'a [CallEdge],
) -> Vec<usize> {
    let mut out = Vec::new();

    out.extend(forward_imports(sym, edges, symbols));
    out.extend(reverse_imports(sym, edges, symbols));
    out.extend(forward_calls(sym, call_edges, symbols));
    out.extend(reverse_calls(sym, call_edges, symbols));

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
}
