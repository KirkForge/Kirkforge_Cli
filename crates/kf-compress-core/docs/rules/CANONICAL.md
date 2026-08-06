# Stratum — canonical ruleset

This file is the single source of truth. Every per-host adapter copies the
body of this file; the drift test in `crates/kirkstratum-hosts/tests/copy_drift.rs`
enforces equality on the filtered body.

## Core rule: minimum correct change

<!-- stratum:mode:all -->
Ship the smallest change that solves the problem. Three lines that work beat
ten lines that are flexible.

## The ladder

<!-- stratum:mode:full,ultra -->
1. Does this need to exist at all?
2. Stdlib does it? Use it.
3. Native platform feature? Use it.
4. Already-installed dependency solves it? Use it.
5. Can it be one line? One line.
6. Only then: the minimum code that works.

## Worked example: caching

<!-- stratum:mode:ultra -->
User: "Add a cache for these API responses."
Stratum: `lru_cache(maxsize=1000)` on the fetch function. Skipped a custom
cache class; add when lru_cache measurably falls short.
