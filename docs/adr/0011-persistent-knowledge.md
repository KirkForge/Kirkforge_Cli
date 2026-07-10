# ADR-0011: Persistent knowledge — saved findings (DEFERRED)

- **Status:** Deferred
- **Date:** 2026-06-24

## Context

A long session produces findings the user wants to keep:
"the bug is in `parse_query`", "the test fixture uses port
8081", "the agent prefers `rg` over `grep`". These findings
are valuable but ephemeral — they live in the conversation
and disappear at session end.

Plugin1's `ReducedStatePacket` (per Plugin1 ADR-002) is the
inspiration: a structured dump of task state that survives
session restart. Plugin3 could write a similar artefact:
*saved findings* that survive a compaction.

Three reasons to defer:

1. The MVP scope is slicing + budget. Adding persistent
   knowledge is a third feature; the budget auto-intervention
   is the load-bearing one and must ship first.
2. Persistent knowledge needs an LLM to extract findings from
   conversation turns; that is a non-trivial design (what to
   extract, how to deduplicate, when to surface). The MVP is
   deterministic-only.
3. The user-facing question "what should Plugin3 remember?" is
   unsolved. A user who installs the MVP may not want any
   persistent artefacts.

## Decision

This ADR documents the *deferred* design so a future
contributor has a starting point.

### Directory layout

```
.plugin3/
└── knowledge/
    ├── findings.jsonl     # one finding per line
    ├── index.toml         # tag → key index
    └── snapshots/
        └── YYYY-MM-DD.jsonl  # daily snapshots
```

The `.plugin3/` directory lives at the workspace root
(detected via `.git/` or `.plugin3/anchor`). A user with
multiple workspaces has one knowledge directory per workspace.

### Finding schema

```rust
#[derive(Serialize, Deserialize)]
pub struct Finding {
    pub id: String,             // BLAKE3 hash of (session_id, ts, body)
    pub ts: DateTime<Utc>,
    pub session_id: String,
    pub kind: FindingKind,      // Code, Config, Decision, Gotcha
    pub body: String,
    pub tags: Vec<String>,
    pub refs: Vec<String>,      // file paths, URLs, line numbers
    pub confidence: f64,        // 0.0 - 1.0
}

#[derive(Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum FindingKind {
    Code,       // a code observation
    Config,     // a configuration detail
    Decision,   // an architectural decision
    Gotcha,     // a non-obvious behaviour
}
```

### When to extract

A future ADR specifies the extraction pipeline. Two candidates:

1. **Local heuristics** — pattern-match on user prompts and
   tool outputs for "the X is Y" sentences. Cheap, low recall.
2. **LLM extraction** — at session end (or on
   UserPromptSubmit), call a small model to extract findings
   from the recent N turns. Better recall, costs tokens.

The MVP is local-only. LLM extraction is a separate ADR.

### When to surface

A future ADR specifies the surfacing pipeline. The hook
candidates:

- **UserPromptSubmit** — before sending the user's prompt,
  embed the top-K most-relevant findings into the prompt.
  Risks bloat; needs relevance ranking.
- **PostToolUse** — after a tool result, append a "related
  findings" footnote. Lower risk; lower utility.

The MVP does neither. Findings are write-only until a future
ADR adds read paths.

### Storage backend

The findings file is JSONL. A SQLite backend is a future ADR
when the file grows beyond ~10 MB and lookup-by-tag becomes
slow.

## Consequences

Negative first:

- The deferred status means a user who wants persistent
  knowledge cannot get it from Plugin3 today. A future
  contributor picks up this ADR and designs the extraction
  + surfacing pipelines.

Positive:

- The MVP ships smaller. Slicing + budget alone is a
  meaningful win; adding knowledge would delay the MVP.
- The directory layout is documented. A future contributor
  does not start from scratch.
- The schema is concrete. `Finding` and `FindingKind` are
  fully specified; the open questions are *when* and *how*,
  not *what*.

## Implementation notes

This ADR is a placeholder. No code lands in the MVP for
persistent knowledge. The `.plugin3/knowledge/` directory is
*not* created by the MVP's `init` flow.

A future contributor who picks up this ADR should:

1. Decide between local heuristics and LLM extraction
   (probably both, with a config flag).
2. Decide on the surfacing policy (UserPromptSubmit is
   higher-value, PostToolUse is lower-risk).
3. Add a `knowledge` feature gate to `plugin3-core` so the
   MVP build does not pull in the LLM extraction dependency.
4. Add tests for the extraction + surfacing pipelines.

The ADR will be promoted from `Deferred` to `Accepted` once
the design questions above are answered.

### Open questions for the future contributor

1. Where does the session boundary live? A user may run
   multiple sessions in one workspace; findings should
   probably be tagged with session_id but visible across
   sessions.
2. How does a finding expire? The MVP has no expiry. A
   workspace that runs for a year accumulates stale findings.
   A future ADR adds a TTL or manual pruning.
3. How does a user opt out? A user who does not want findings
   should be able to disable them in `config.toml`. The MVP
   disables by default; opting in is explicit.