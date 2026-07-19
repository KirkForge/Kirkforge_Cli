use crate::cli::EnvSource;
use anyhow::Context;
use kirkstratum_core::config::DEFAULT_TOML;
use std::path::PathBuf;

/// Create the default `pipeline.toml` in the requested config directory.
///
/// If `config_dir` is `None`, the file is written to the XDG config home under
/// `stratum/`. Existing files are preserved unless `force` is `true`. On Unix
/// the file is created with `0o600` permissions so host-specific tuning is not
/// world-readable.
#[must_use = "the created config path should be reported to the user"]
pub fn initialise_config(
    env: &dyn EnvSource,
    config_dir: Option<&std::path::Path>,
    force: bool,
) -> anyhow::Result<PathBuf> {
    let dir = if let Some(dir) = config_dir {
        dir.to_path_buf()
    } else {
        env.config_home()
            .ok_or_else(|| {
                anyhow::anyhow!("cannot determine config home; set XDG_CONFIG_HOME or HOME")
            })?
            .join("stratum")
    };

    let path = dir.join("pipeline.toml");
    if path.exists() && !force {
        return Err(anyhow::anyhow!(
            "{} already exists; use --force to overwrite",
            path.display()
        ));
    }

    // Only create the directory (and set restrictive permissions) when we are
    // actually going to write the file. This avoids side effects when init is
    // called on an existing config without --force.
    #[cfg(unix)]
    let dir_existed = dir.exists();
    #[cfg(not(unix))]
    let _dir_existed = ();
    std::fs::create_dir_all(&dir)
        .with_context(|| format!("cannot create config directory {}", dir.display()))?;

    // Restrict the config directory on Unix so other users cannot list or
    // traverse it. The file itself is locked down below; this hardens the path
    // leading to it. Only change permissions on directories we created so we
    // do not alter intentionally different permissions on existing paths.
    #[cfg(unix)]
    if !dir_existed {
        use std::os::unix::fs::PermissionsExt;
        let perms = std::fs::Permissions::from_mode(0o700);
        std::fs::set_permissions(&dir, perms)
            .with_context(|| format!("cannot set permissions on {}", dir.display()))?;
    }

    std::fs::write(&path, DEFAULT_TOML)
        .with_context(|| format!("cannot write config to {}", path.display()))?;

    // Restrict config file permissions on Unix so other users cannot read
    // host-specific overrides or tuning values. Windows ACLs are left to the
    // OS default; this is the common cross-platform compromise.
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        let perms = std::fs::Permissions::from_mode(0o600);
        std::fs::set_permissions(&path, perms)
            .with_context(|| format!("cannot set permissions on {}", path.display()))?;
    }

    Ok(path)
}
