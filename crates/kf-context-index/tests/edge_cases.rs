// WO 8.9 — Context index edge-case regression tests.
//
// The five fixtures in `tests/fixtures/` exercise gaps in the original
// tree-sitter extraction. Each test runs `extract_symbols` (via
// `index_file`) on a fixture and asserts the right symbol list.

use std::path::PathBuf;

use kf_context_index::ContextIndex;

fn fixture(name: &str) -> (PathBuf, String) {
    let path = PathBuf::from(env!("CARGO_MANIFEST_DIR"))
        .join("tests/fixtures")
        .join(name);
    let content = std::fs::read_to_string(&path).expect("fixture must exist on disk");
    (path, content)
}

fn index_fixture(name: &str) -> ContextIndex {
    let (path, content) = fixture(name);
    let mut idx = ContextIndex::new();
    idx.index_file(&path, &content)
        .expect("index_file should not fail on fixture");
    idx
}

// WO 8.9 edge case 1: TypeScript arrow function exports.
#[test]
fn ts_arrow_function_export_extracted() {
    let idx = index_fixture("ts_arrow.ts");
    let syms = idx.symbols();
    let names: Vec<&str> = syms.iter().map(|s| s.name.as_str()).collect();

    // The two arrow function exports should now be symbols.
    assert!(
        names.contains(&"foo"),
        "expected `foo` from `export const foo = () => {{}}`, got {names:?}"
    );
    assert!(
        names.contains(&"bar"),
        "expected `bar` from `export const bar = (x: number) => x * 2`, got {names:?}"
    );

    // The non-arrow const is a plain value, not a function — should not
    // appear as a Function symbol under the new logic.
    assert!(
        !names.contains(&"baz"),
        "expected `baz` (plain number const) to NOT be a symbol, got {names:?}"
    );

    // The declared function is unchanged.
    assert!(
        names.contains(&"declared"),
        "expected `declared` from `export function declared() {{}}`, got {names:?}"
    );
}

// WO 8.9 edge case 2: TypeScript interface merging.
#[test]
fn ts_interface_merge_dedupes_within_file() {
    let mut idx = index_fixture("ts_interface_merge.ts");
    // The fixture has two `interface Foo {}` declarations and one
    // `interface Bar {}` declaration. Before dedup: 3 interface entries.
    // After dedup: 2 (one Foo, one Bar).
    let before = idx
        .symbols()
        .iter()
        .filter(|s| s.kind == kf_context_index::SymbolKind::Interface)
        .count();
    assert_eq!(
        before, 3,
        "fixture should yield 3 interface entries before dedup (2 Foo + 1 Bar), got {before}"
    );
    idx.dedup_interfaces();
    let after: Vec<&str> = idx
        .symbols()
        .iter()
        .filter(|s| s.kind == kf_context_index::SymbolKind::Interface)
        .map(|s| s.name.as_str())
        .collect();
    assert_eq!(
        after,
        vec!["Foo", "Bar"],
        "dedup should keep one Foo and one Bar, got {after:?}"
    );
}

#[test]
fn ts_interface_merge_dedup_keeps_one_per_file() {
    // Same fixture, same file -> the two `Foo` declarations dedupe to one.
    let mut idx = index_fixture("ts_interface_merge.ts");
    idx.dedup_interfaces();
    let foo_entries: Vec<&kf_context_index::Symbol> = idx
        .symbols()
        .iter()
        .filter(|s| s.name == "Foo" && s.kind == kf_context_index::SymbolKind::Interface)
        .collect();
    assert_eq!(
        foo_entries.len(),
        1,
        "expected exactly one Foo per (name, file) after dedup, got {foo_entries:?}"
    );
}

// WO 8.9 edge case 3: Python decorator-wrapped functions. The current
// walker skips children of `decorated_definition`, so the function name
// (not the decorator) is captured. This test locks in that behavior.
#[test]
fn py_decorator_function_extracted_by_name() {
    let idx = index_fixture("py_decorator.py");
    let syms: Vec<&kf_context_index::Symbol> = idx
        .symbols()
        .iter()
        .filter(|s| s.kind == kf_context_index::SymbolKind::Function)
        .collect();
    let names: Vec<&str> = syms.iter().map(|s| s.name.as_str()).collect();

    assert!(
        names.contains(&"handler"),
        "expected `handler` (the function under @app.route), got {names:?}"
    );
    assert!(
        names.contains(&"helper"),
        "expected `helper` (the function under @staticmethod), got {names:?}"
    );
    assert!(
        names.contains(&"users"),
        "expected `users` (the function under stacked decorators), got {names:?}"
    );

    // No decorator name should leak in as a symbol.
    assert!(
        !names.contains(&"route"),
        "decorator name `route` must not appear as a function symbol, got {names:?}"
    );
    assert!(
        !names.contains(&"app"),
        "decorator receiver `app` must not appear as a function symbol, got {names:?}"
    );
}

// WO 8.9 edge case 4: Python `if __name__ == "__main__":` blocks.
#[test]
fn py_dunder_main_guard_does_not_create_symbols() {
    let idx = index_fixture("py_dunder.py");
    let syms: Vec<&kf_context_index::Symbol> = idx
        .symbols()
        .iter()
        .filter(|s| s.kind == kf_context_index::SymbolKind::Function)
        .collect();
    let names: Vec<&str> = syms.iter().map(|s| s.name.as_str()).collect();

    // The two module-level functions ARE symbols.
    assert!(
        names.contains(&"main"),
        "expected module-level `main`, got {names:?}"
    );
    assert!(
        names.contains(&"helper"),
        "expected module-level `helper`, got {names:?}"
    );

    // Nothing inside the `if __name__ == "__main__":` block should leak
    // into the symbol list. The body is a series of expression statements
    // (function calls + a print), none of which are function defs; the
    // real test is that walking past the guard does not pick up the
    // block's locals as if they were module-level symbols.
    //
    // Additionally: before the WO 8.9 fix, the body of the guard
    // produced spurious symbols (e.g. any `def` inside it would be
    // captured as a top-level Function even though it's behind a
    // `__main__` guard). This fixture's body is pure calls, but the
    // assertion still locks in "the guard body is skipped" by
    // requiring that the ONLY function symbols come from module-level
    // definitions.
    assert_eq!(
        syms.len(),
        2,
        "expected exactly 2 function symbols (main, helper), got {syms:?}"
    );
}

#[test]
fn py_dunder_main_guard_with_inner_def() {
    // If a `def` appears INSIDE the `if __name__` block, the WO 8.9
    // rule says: skip the body entirely. That inner `def` is not a
    // module-level symbol and should NOT appear.
    let src = "def outer():\n    pass\n\nif __name__ == \"__main__\":\n    def inner():\n        pass\n    inner()\n";
    let mut idx = ContextIndex::new();
    let path = PathBuf::from("/tmp/inner_def.py");
    idx.index_file(&path, src).unwrap();
    let names: Vec<&str> = idx.symbols().iter().map(|s| s.name.as_str()).collect();
    assert!(
        names.contains(&"outer"),
        "expected module-level outer, got {names:?}"
    );
    assert!(
        !names.contains(&"inner"),
        "inner def inside `if __name__` must not be a symbol, got {names:?}"
    );
}

// WO 8.9 edge case 5: Go method receivers.
#[test]
fn go_method_receiver_included_in_name() {
    let idx = index_fixture("go_method.go");
    let syms: Vec<&kf_context_index::Symbol> = idx
        .symbols()
        .iter()
        .filter(|s| s.kind == kf_context_index::SymbolKind::Function)
        .collect();
    let names: Vec<&str> = syms.iter().map(|s| s.name.as_str()).collect();

    // `func (s *Server) Start()` -> "Server.Start" (pointer receiver
    // is normalized to the base type).
    assert!(
        names.contains(&"Server.Start"),
        "expected `Server.Start` for `func (s *Server) Start()`, got {names:?}"
    );
    // `func (r Server) Stop()` -> "Server.Stop" (value receiver).
    assert!(
        names.contains(&"Server.Stop"),
        "expected `Server.Stop` for `func (r Server) Stop()`, got {names:?}"
    );
    // `func plain()` has no receiver -> plain name unchanged.
    assert!(
        names.contains(&"plain"),
        "expected `plain` for a non-method function, got {names:?}"
    );
    // No bare "Start" / "Stop" should appear.
    assert!(
        !names.contains(&"Start"),
        "bare `Start` (without receiver prefix) must not appear, got {names:?}"
    );
    assert!(
        !names.contains(&"Stop"),
        "bare `Stop` (without receiver prefix) must not appear, got {names:?}"
    );
}

#[test]
fn mtime_rebuild_noop_is_cheap() {
    let tmp = tempfile::tempdir().unwrap();
    let repo_root = tmp.path();

    let src_path = tmp.path().join("lib.rs");
    std::fs::write(&src_path, "fn hello() {}\nfn world() {}\n").unwrap();

    let mut idx = ContextIndex::new();
    idx.index_dir(repo_root).unwrap();

    let cache_path = tmp.path().join("cache.json");
    let head = "0000000000000000000000000000000000000000".to_string();
    idx.save(&cache_path, &head).unwrap();

    let cached = ContextIndex::load(&cache_path).unwrap();
    let start = std::time::Instant::now();
    let (rebuilt, changed) = ContextIndex::mtime_rebuild(cached, repo_root);
    let elapsed = start.elapsed();

    assert_eq!(changed, 0, "no files changed, so changed count must be 0");
    assert_eq!(
        rebuilt.symbols().len(),
        2,
        "both symbols should survive no-op rebuild"
    );
    assert!(
        elapsed.as_millis() < 100,
        "no-op mtime rebuild should be <100ms, took {}ms",
        elapsed.as_millis()
    );
}

#[test]
fn mtime_rebuild_single_file_change() {
    let tmp = tempfile::tempdir().unwrap();
    let repo_root = tmp.path();

    let a_path = tmp.path().join("a.rs");
    let b_path = tmp.path().join("b.rs");
    std::fs::write(&a_path, "fn alpha() {}\n").unwrap();
    std::fs::write(&b_path, "fn beta() {}\n").unwrap();

    let mut idx = ContextIndex::new();
    idx.index_dir(repo_root).unwrap();
    assert_eq!(idx.symbols().len(), 2);

    let cache_path = tmp.path().join("cache.json");
    let head = "0000000000000000000000000000000000000000".to_string();
    idx.save(&cache_path, &head).unwrap();

    std::fs::write(&a_path, "fn alpha_v2() {}\nfn new_func() {}\n").unwrap();

    // Force a distinct mtime deterministically without a wall-clock sleep.
    // Filesystems may round mtimes to whole seconds, so bump by 2s to be safe.
    //
    // The file is opened with write access because Windows `SetFileTime`
    // (backing `set_modified`) requires a handle with GENERIC_WRITE — a
    // read-only `File::open` handle yields `ERROR_ACCESS_DENIED`. On Unix
    // `futimens` does not require write access for the owner, so the write
    // access is harmless there.
    let later = std::time::SystemTime::now() + std::time::Duration::from_secs(2);
    let file = std::fs::OpenOptions::new()
        .write(true)
        .open(&a_path)
        .unwrap();
    file.set_modified(later).unwrap();
    drop(file);

    let cached = ContextIndex::load(&cache_path).unwrap();
    let (rebuilt, changed) = ContextIndex::mtime_rebuild(cached, repo_root);

    assert_eq!(changed, 1, "only a.rs changed");
    let names: Vec<&str> = rebuilt.symbols().iter().map(|s| s.name.as_str()).collect();
    assert!(
        names.contains(&"alpha_v2"),
        "updated function should appear, got {names:?}"
    );
    assert!(
        names.contains(&"new_func"),
        "new function should appear, got {names:?}"
    );
    assert!(
        names.contains(&"beta"),
        "unchanged file's symbol should survive, got {names:?}"
    );
    assert!(
        !names.contains(&"alpha"),
        "old function should be removed, got {names:?}"
    );
}
