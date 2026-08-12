// WO 28.1: toolset type definitions moved to `tools::toolset` — they are the
// tools' own composition primitives and belong with `tools`, not `session`.
// This file is now the legacy re-export so the many `session::toolset` callers
// (executor, run_session, tui, line_mode, tests) keep resolving. A future cleanup
// can repoint those callers at `tools::toolset` directly and delete this shim.
pub use crate::tools::toolset::{CompositeToolset, Toolset, VecToolset};
