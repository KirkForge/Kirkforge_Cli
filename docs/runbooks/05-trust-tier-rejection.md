# Runbook: Plugin Trust-Tier Rejection

## Symptom

- Plugin fails to load with `ManifestError` or a `TrustPolicy` rejection
- Logs show the plugin requesting a trust tier higher than the host's configured `max_plugin_trust`, or `verification policy rejected unsigned plugin`
- The plugin is absent from the host's loaded plugin set

## Diagnosis

1. Read the plugin's `kirkforge.toml` and note the `trust` field.
2. Check the host's `TrustPolicy` configuration (set in code where the host embeds KirkForge):
   - `max_plugin_trust` — the highest tier the host will grant
   - `verification` — `PluginVerificationPolicy::allow_unsigned()` or `require_signed_with_keys([...])`
3. If the plugin declares `public-key`, confirm that key is in the allowlist passed to `require_signed_with_keys`.

## Resolution

- **Tier too high**: lower the plugin's `trust` to match what it actually needs, or raise the host's `max_plugin_trust` (operator decision — review `SECURITY.md` first).
- **Unsigned plugin rejected**: only run unsigned plugins in dev. Construct the host `TrustPolicy` with `PluginVerificationPolicy::allow_unsigned()` for the dev session; never use this in production.
- **Key not trusted**: add the plugin's Ed25519 public key to the allowlist passed to `require_signed_with_keys`, or re-sign the plugin with a key already in the allowlist.

## Prevention

- Production deployments should construct `TrustPolicy` with `require_signed_with_keys`; unsigned is opt-in for dev only.
- Document each plugin's required trust tier in its `kirkforge.toml` `description` field.
- Review `crates/kirkforge-plugin/src/verification.rs` for the verification contract before promoting a plugin.