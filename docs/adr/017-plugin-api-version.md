# ADR 017: Plugin API version contract

## Status

Accepted

## Context

The plugin manifest currently declares `name`, `version`, and `trust`, but there is no field that tells the host which plugin API contract the manifest follows. This makes it impossible to introduce breaking changes later without guessing based on the presence or absence of fields.

## Decision

Add an `api_version` field to `PluginManifest` with a default value of `v1`. The host validates this field at load time and rejects any plugin that declares a version other than `v1`.

## Consequences

- Plugin authors can opt into future API versions explicitly.
- The host never silently misinterprets a manifest written for a newer API.
- v1 plugins remain compatible without any manifest changes because `api_version` defaults to `v1`.

## Stability guarantee

v1 is the current stable API. Future major manifest changes will introduce new `api_version` variants. Within a major version, new optional fields may be added, but required fields and capability semantics will not change.
