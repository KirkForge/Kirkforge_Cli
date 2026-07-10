//! Plugin signature and integrity verification.
//!
//! v1 plugins support detached Ed25519 signatures. The plugin manifest
//! declares a public key (hex), and a separate `kirkforge.sig` file in the
//! plugin directory contains the hex-encoded signature of the raw
//! `kirkforge.toml` bytes.
//!
//! The host decides which public keys are trustworthy via an allowlist. A
//! signature only proves that a manifest was signed by *some* key; the
//! allowlist proves that key is authorized.

use ed25519_dalek::Verifier;
use std::path::Path;

const SIGNATURE_FILE: &str = "kirkforge.sig";
const PUBLIC_KEY_BYTES: usize = 32;
const SIGNATURE_BYTES: usize = 64;

/// Policy controlling how strictly the host verifies plugin signatures.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct PluginVerificationPolicy {
    /// Hex-encoded Ed25519 public keys the host trusts to sign plugins.
    pub trusted_public_keys: Vec<String>,
    /// If true, plugins without a valid signature from a trusted key are
    /// rejected. If false, unsigned plugins are allowed but a present, invalid
    /// signature is still rejected.
    pub require_signed: bool,
}

impl PluginVerificationPolicy {
    /// Accept unsigned plugins and ignore signatures entirely.
    pub fn allow_unsigned() -> Self {
        Self {
            trusted_public_keys: Vec::new(),
            require_signed: false,
        }
    }

    /// Require a valid signature from one of the provided trusted keys.
    pub fn require_signed_with_keys(keys: impl IntoIterator<Item = String>) -> Self {
        Self {
            trusted_public_keys: keys.into_iter().collect(),
            require_signed: true,
        }
    }
}

/// Errors that can occur while verifying a plugin signature.
#[derive(Debug, thiserror::Error)]
pub enum PluginVerificationError {
    #[error("plugin signing is required but no signature file was found")]
    SignatureRequired,
    #[error("signature file not found: {0}")]
    MissingSignature(std::path::PathBuf),
    #[error("cannot read signature file: {0}")]
    Io(#[from] std::io::Error),
    #[error("plugin public key is not in the host trust allowlist")]
    UntrustedKey,
    #[error("plugin declares no public_key and host requires signatures")]
    MissingPublicKey,
    #[error("invalid public key hex: {0}")]
    InvalidPublicKey(String),
    #[error("invalid signature hex: {0}")]
    InvalidSignature(String),
    #[error("signature verification failed: manifest may have been tampered with")]
    BadSignature,
}

/// Verify a plugin's detached Ed25519 signature.
///
/// `plugin_root` is the plugin directory. `manifest_bytes` must be the raw
/// bytes of the `kirkforge.toml` file that was signed. `public_key` is taken
/// from the parsed manifest. The host policy controls whether verification is
/// required and which keys are trusted.
///
/// Returns `Ok(())` if the plugin is accepted, or an error if verification
/// fails or the plugin is unsigned while signatures are required.
pub fn verify_plugin_manifest(
    plugin_root: &Path,
    manifest_bytes: &[u8],
    public_key: Option<&str>,
    policy: &PluginVerificationPolicy,
) -> Result<(), PluginVerificationError> {
    let sig_path = plugin_root.join(SIGNATURE_FILE);

    if !sig_path.exists() {
        if policy.require_signed {
            return Err(PluginVerificationError::SignatureRequired);
        }
        // Unsigned plugin is allowed.
        return Ok(());
    }

    let sig_hex = std::fs::read_to_string(&sig_path)?;
    let sig_hex = sig_hex.trim();

    let public_key = public_key.ok_or(PluginVerificationError::MissingPublicKey)?;

    if !policy.trusted_public_keys.iter().any(|k| k == public_key) {
        return Err(PluginVerificationError::UntrustedKey);
    }

    let public_key_bytes = hex::decode(public_key)
        .map_err(|e| PluginVerificationError::InvalidPublicKey(e.to_string()))?;
    if public_key_bytes.len() != PUBLIC_KEY_BYTES {
        return Err(PluginVerificationError::InvalidPublicKey(format!(
            "expected {} bytes, got {}",
            PUBLIC_KEY_BYTES,
            public_key_bytes.len()
        )));
    }

    let sig_bytes = hex::decode(sig_hex)
        .map_err(|e| PluginVerificationError::InvalidSignature(e.to_string()))?;
    if sig_bytes.len() != SIGNATURE_BYTES {
        return Err(PluginVerificationError::InvalidSignature(format!(
            "expected {} bytes, got {}",
            SIGNATURE_BYTES,
            sig_bytes.len()
        )));
    }

    let verifying_key = ed25519_dalek::VerifyingKey::from_bytes(
        &public_key_bytes.try_into().expect("length checked above"),
    )
    .map_err(|e| PluginVerificationError::InvalidPublicKey(e.to_string()))?;

    let signature =
        ed25519_dalek::Signature::from_bytes(&sig_bytes.try_into().expect("length checked above"));

    verifying_key
        .verify(manifest_bytes, &signature)
        .map_err(|_| PluginVerificationError::BadSignature)?;

    Ok(())
}

/// Compute a SHA-256 hex digest of arbitrary bytes.
#[allow(dead_code)]
fn sha256_hex(bytes: &[u8]) -> String {
    use sha2::{Digest, Sha256};
    let mut hasher = Sha256::new();
    hasher.update(bytes);
    hex::encode(hasher.finalize())
}

#[cfg(test)]
mod tests {
    use super::*;
    use ed25519_dalek::{Signer, SigningKey};
    use rand::rngs::OsRng;

    fn generate_keypair() -> (SigningKey, String) {
        let signing_key = SigningKey::generate(&mut OsRng);
        let verifying_key = signing_key.verifying_key();
        let hex_key = hex::encode(verifying_key.to_bytes());
        (signing_key, hex_key)
    }

    #[test]
    fn unsigned_plugin_allowed_when_not_required() {
        let tmp = tempfile::tempdir().unwrap();
        let policy = PluginVerificationPolicy::allow_unsigned();
        assert!(verify_plugin_manifest(tmp.path(), b"x", None, &policy).is_ok());
    }

    #[test]
    fn unsigned_plugin_rejected_when_required() {
        let tmp = tempfile::tempdir().unwrap();
        let policy = PluginVerificationPolicy::require_signed_with_keys([]);
        assert!(matches!(
            verify_plugin_manifest(tmp.path(), b"x", None, &policy),
            Err(PluginVerificationError::SignatureRequired)
        ));
    }

    #[test]
    fn valid_signature_accepted() {
        let tmp = tempfile::tempdir().unwrap();
        let manifest = b"name = 'test'\n";
        let (signing_key, hex_key) = generate_keypair();
        let signature = signing_key.sign(manifest);
        std::fs::write(
            tmp.path().join(SIGNATURE_FILE),
            hex::encode(signature.to_bytes()),
        )
        .unwrap();

        let policy = PluginVerificationPolicy::require_signed_with_keys([hex_key.clone()]);
        assert!(verify_plugin_manifest(tmp.path(), manifest, Some(&hex_key), &policy).is_ok());
    }

    #[test]
    fn untrusted_key_rejected() {
        let tmp = tempfile::tempdir().unwrap();
        let manifest = b"name = 'test'\n";
        let (signing_key, _hex_key) = generate_keypair();
        let (_other, other_hex) = generate_keypair();
        let signature = signing_key.sign(manifest);
        std::fs::write(
            tmp.path().join(SIGNATURE_FILE),
            hex::encode(signature.to_bytes()),
        )
        .unwrap();

        let policy = PluginVerificationPolicy::require_signed_with_keys([other_hex]);
        assert!(matches!(
            verify_plugin_manifest(
                tmp.path(),
                manifest,
                Some(&hex::encode(signing_key.verifying_key().to_bytes())),
                &policy
            ),
            Err(PluginVerificationError::UntrustedKey)
        ));
    }

    #[test]
    fn tampered_manifest_rejected() {
        let tmp = tempfile::tempdir().unwrap();
        let manifest = b"name = 'test'\n";
        let (signing_key, hex_key) = generate_keypair();
        let signature = signing_key.sign(manifest);
        std::fs::write(
            tmp.path().join(SIGNATURE_FILE),
            hex::encode(signature.to_bytes()),
        )
        .unwrap();

        let policy = PluginVerificationPolicy::require_signed_with_keys([hex_key.clone()]);
        assert!(matches!(
            verify_plugin_manifest(tmp.path(), b"name = 'evil'\n", Some(&hex_key), &policy),
            Err(PluginVerificationError::BadSignature)
        ));
    }
}
