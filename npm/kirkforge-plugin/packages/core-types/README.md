# @kirkforge/core-types

Shared TypeScript types for the KirkForge monorepo. Provides the `Result<T,E>` pattern used throughout all packages.

## Key exports

- `Result<T, E>` — Ok/Err discriminated union
- `ok(value)`, `err(error)` — constructor helpers
