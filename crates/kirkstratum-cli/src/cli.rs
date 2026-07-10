use crate::config_source::ConfigSource;
use clap::builder::{PossibleValuesParser, TypedValueParser};
use clap::{Parser, Subcommand};
use kirkstratum_core::config::{ConfigError, PipelineConfig};
use kirkstratum_core::content::ContentType;
use kirkstratum_core::mode::{Mode, ALL_MODES};
use std::path::PathBuf;

/// Trait for environment variable access so resolution can be tested without
/// touching real process state.
pub trait EnvSource: Send + Sync {
    fn config_home(&self) -> Option<PathBuf>;
}

/// Production environment source backed by `std::env`.
#[derive(Debug, Clone, Copy, Default)]
pub struct ProcessEnv;

impl EnvSource for ProcessEnv {
    fn config_home(&self) -> Option<PathBuf> {
        std::env::var_os("XDG_CONFIG_HOME")
            .map(PathBuf::from)
            .or_else(|| {
                std::env::var_os("HOME").map(|home| {
                    let mut p = PathBuf::from(home);
                    p.push(".config");
                    p
                })
            })
    }
}

fn mode_parser() -> impl clap::builder::TypedValueParser<Value = Mode> {
    PossibleValuesParser::new(ALL_MODES.iter().map(|m| m.as_str()))
        .try_map(|s: String| s.parse::<Mode>())
}

fn content_type_parser() -> impl clap::builder::TypedValueParser<Value = ContentType> {
    PossibleValuesParser::new(ContentType::ALL.iter().map(|ct| ct.as_str()))
        .try_map(|s: String| s.parse::<ContentType>())
}

fn positive_usize(s: &str) -> Result<usize, String> {
    s.parse::<usize>().map_err(|e| e.to_string()).and_then(|n| {
        if n == 0 {
            Err("must be at least 1".to_string())
        } else {
            Ok(n)
        }
    })
}

#[derive(Parser, Debug)]
#[command(name = "stratum", version = env!("CARGO_PKG_VERSION"))]
/// Top-level command-line arguments for the `stratum` binary.
pub struct Cli {
    /// Path to a TOML config file that overrides defaults.
    #[arg(long, global = true, env = "STRATUM_CONFIG")]
    pub config: Option<PathBuf>,

    /// Path to config directory (default: `$XDG_CONFIG_HOME/stratum`).
    #[arg(long, global = true, env = "STRATUM_CONFIG_DIR")]
    pub config_dir: Option<PathBuf>,

    /// Token budget used by the bloat heuristic.
    #[arg(long, global = true, env = "STRATUM_TOKEN_BUDGET", value_parser = positive_usize)]
    pub token_budget: Option<usize>,

    /// Emit machine-readable JSON to stdout where applicable.
    #[arg(long, global = true)]
    pub json: bool,

    /// Maximum input size in bytes (default: 50 MiB).
    #[arg(long, global = true, env = "STRATUM_MAX_INPUT_SIZE", value_parser = positive_usize)]
    pub max_input_size: Option<usize>,

    /// Pipeline mode for this invocation (off, lite, full, ultra).
    #[arg(long, global = true, env = "STRATUM_MODE", value_parser = mode_parser())]
    pub mode: Option<Mode>,

    /// Increase verbosity (-v, -vv, -vvv).
    #[arg(short, long, action = clap::ArgAction::Count, global = true)]
    pub verbose: u8,

    /// Decrease verbosity.
    #[arg(short, long, action = clap::ArgAction::Count, global = true)]
    pub quiet: u8,

    /// Preview what the pipeline would do without emitting transformed output.
    #[arg(long, global = true)]
    pub dry_run: bool,

    #[command(subcommand)]
    pub command: Command,
}

#[derive(Subcommand, Debug)]
/// Available `stratum` subcommands.
pub enum Command {
    /// Run the pipeline on stdin.
    Run,
    /// Apply the pipeline to stdin or a file (debugging).
    Apply {
        /// File to read from; default is stdin.
        file: Option<PathBuf>,

        /// Force content type detection (skip layered detection).
        #[arg(long, value_parser = content_type_parser())]
        content_type: Option<ContentType>,

        /// Override the mode for this invocation.
        #[arg(long, value_parser = mode_parser())]
        mode: Option<Mode>,
    },
    /// Show or set the active mode.
    Mode {
        /// Optional mode value to set for this invocation.
        #[arg(value_parser = mode_parser())]
        value: Option<Mode>,
    },
    /// Emit the ruleset for the active mode.
    Rules {
        /// Mode to emit rules for (default: full).
        #[arg(long, value_parser = mode_parser())]
        mode: Option<Mode>,
    },
    /// Print version and exit.
    Version,
    /// Inspect the effective config.
    Config {
        /// Validate config and exit non-zero on errors.
        #[arg(long, conflicts_with = "sources")]
        validate: bool,

        /// Show the sources that contributed to the effective config.
        #[arg(long, conflicts_with = "validate")]
        sources: bool,
    },
    /// Emit shell completion script for the given shell.
    Completion {
        /// Target shell.
        shell: clap_complete::Shell,
    },
    /// Initialise the default config in `$XDG_CONFIG_HOME/stratum/`.
    Init {
        /// Overwrite an existing config file.
        #[arg(long)]
        force: bool,
    },
}

/// XDG default override path for the pipeline config, if a config dir exists.
#[must_use]
pub fn xdg_config_path(env: &dyn EnvSource) -> Option<PathBuf> {
    let mut path = env.config_home()?;
    path.push("stratum");
    path.push("pipeline.toml");
    Some(path)
}

/// Load the effective pipeline config and record which sources contributed.
///
/// Precedence (highest to lowest):
/// 1. `STRATUM_CONFIG` env var (also bound to `--config`)
/// 2. `--config-dir/pipeline.toml`
/// 3. XDG default override (`$XDG_CONFIG_HOME/stratum/pipeline.toml`)
/// 4. Embedded default
///
/// Note: clap binds `--config` to `STRATUM_CONFIG`, so the CLI flag and env var
/// share the same value. The env var wins when both are present only if set
/// outside of the flag; in practice clap resolves the env/flag binding before
/// `load_config` is called.
#[must_use = "the loaded config and sources are required to run the pipeline"]
pub fn load_config(
    cli: &Cli,
    env: &dyn EnvSource,
) -> Result<(PipelineConfig, Vec<ConfigSource>), ConfigError> {
    let mut cfg = PipelineConfig::default();
    let mut sources = vec![ConfigSource::Embedded];

    // Pick the highest-precedence existing config file (only one file layer is
    // loaded because later layers completely override earlier ones).
    let file_layer = cli
        .config
        .as_ref()
        .map(|path| {
            let source = ConfigSource::Explicit { path: path.clone() };
            (path.clone(), source)
        })
        .or_else(|| {
            cli.config_dir.as_ref().and_then(|dir| {
                let path = dir.join("pipeline.toml");
                path.exists()
                    .then(|| (path.clone(), ConfigSource::ConfigDir { path }))
            })
        })
        .or_else(|| {
            xdg_config_path(env)
                .filter(|p| p.exists())
                .map(|path| (path.clone(), ConfigSource::Xdg { path }))
        });

    if let Some((path, source)) = file_layer {
        cfg = PipelineConfig::from_file(&path).map_err(|e| {
            // Tag the error with the source that failed so the CLI can report it.
            ConfigError::Invalid {
                field: source.kind().to_string(),
                message: format!("{}: {}", path.display(), e),
            }
        })?;
        sources.push(source);
    }

    Ok((cfg, sources))
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::init::initialise_config;
    use kirkstratum_core::config::Ratio;
    use std::io::Write;
    use tempfile::{NamedTempFile, TempDir};

    struct FakeEnv {
        config_home: Option<PathBuf>,
    }

    impl EnvSource for FakeEnv {
        fn config_home(&self) -> Option<PathBuf> {
            self.config_home.clone()
        }
    }

    fn env_with_config_home() -> (TempDir, PathBuf, FakeEnv) {
        let dir = TempDir::new().unwrap();
        let config_home = dir.path().join(".config");
        std::fs::create_dir_all(config_home.join("stratum")).unwrap();
        let env = FakeEnv {
            config_home: Some(config_home.clone()),
        };
        (dir, config_home, env)
    }

    #[test]
    fn config_flag_overrides_default() {
        let mut file = NamedTempFile::new().unwrap();
        writeln!(file, "bloat_threshold = 0.1").unwrap();

        let cli = Cli::parse_from(["stratum", "--config", file.path().to_str().unwrap(), "run"]);
        let (cfg, sources) = load_config(&cli, &ProcessEnv).unwrap();

        assert_eq!(cfg.bloat_threshold, Ratio::new_unchecked(0.1));
        // Unrelated default field is preserved.
        assert_eq!(cfg.reformat_target_ratio, Ratio::new_unchecked(0.05));
        assert_eq!(sources.len(), 2);
        assert!(matches!(sources[0], ConfigSource::Embedded));
        assert!(matches!(sources[1], ConfigSource::Explicit { .. }));
    }

    #[test]
    fn cli_config_overrides_xdg_config() {
        let (_dir, config_home, env) = env_with_config_home();
        let xdg_path = config_home.join("stratum/pipeline.toml");
        std::fs::write(&xdg_path, "bloat_threshold = 0.2").unwrap();

        let mut file = NamedTempFile::new().unwrap();
        writeln!(file, "bloat_threshold = 0.1").unwrap();

        let cli = Cli::parse_from(["stratum", "--config", file.path().to_str().unwrap(), "run"]);
        let (cfg, sources) = load_config(&cli, &env).unwrap();

        assert_eq!(cfg.bloat_threshold, Ratio::new_unchecked(0.1));
        // Only the explicit config file is loaded; XDG is ignored when a higher
        // precedence source is present.
        assert_eq!(sources.len(), 2);
        assert!(matches!(sources[0], ConfigSource::Embedded));
        assert!(matches!(sources[1], ConfigSource::Explicit { .. }));
    }

    #[test]
    fn env_config_overrides_cli_config() {
        let (_dir, config_home, env) = env_with_config_home();
        let xdg_path = config_home.join("stratum/pipeline.toml");
        std::fs::write(&xdg_path, "bloat_threshold = 0.2").unwrap();

        let mut env_file = NamedTempFile::new().unwrap();
        writeln!(env_file, "bloat_threshold = 0.05").unwrap();

        // Simulate env var being the active source: clap normally resolves the
        // env binding before calling load_config, so we build a Cli with the
        // env value as the flag value directly.
        let cli = Cli {
            config: Some(env_file.path().to_path_buf()),
            config_dir: None,
            token_budget: None,
            json: false,
            max_input_size: None,
            mode: None,
            verbose: 0,
            quiet: 0,
            dry_run: false,
            command: Command::Run,
        };
        let (cfg, sources) = load_config(&cli, &env).unwrap();

        assert_eq!(cfg.bloat_threshold, Ratio::new_unchecked(0.05));
        assert_eq!(sources.len(), 2);
        assert!(matches!(sources[1], ConfigSource::Explicit { .. }));
        assert_eq!(sources[1].kind(), "explicit");
    }

    #[test]
    fn init_config_creates_xdg_file() {
        let dir = TempDir::new().unwrap();
        let config_home = dir.path().join(".config");
        let env = FakeEnv {
            config_home: Some(config_home.clone()),
        };
        let path = config_home.join("stratum/pipeline.toml");

        let written = initialise_config(&env, None, false).unwrap();
        assert_eq!(written, path);
        assert!(path.exists());

        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            let dir_mode = std::fs::metadata(config_home.join("stratum"))
                .unwrap()
                .permissions()
                .mode();
            assert_eq!(
                dir_mode & 0o777,
                0o700,
                "config directory must be owner-only"
            );
            let file_mode = std::fs::metadata(&path).unwrap().permissions().mode();
            assert_eq!(
                file_mode & 0o777,
                0o600,
                "config file must be owner-readable/writable only"
            );
        }
    }

    #[test]
    fn init_config_refuses_overwrite_without_force() {
        let dir = TempDir::new().unwrap();
        let config_home = dir.path().join(".config");
        let env = FakeEnv {
            config_home: Some(config_home.clone()),
        };
        let path = config_home.join("stratum/pipeline.toml");

        std::fs::create_dir_all(path.parent().unwrap()).unwrap();
        std::fs::write(&path, "bloat_threshold = 0.1").unwrap();

        let result = initialise_config(&env, None, false);
        assert!(result.is_err());
    }

    #[test]
    fn config_dir_overrides_xdg_config() {
        let (_dir, config_home, env) = env_with_config_home();
        let xdg_path = config_home.join("stratum/pipeline.toml");
        std::fs::write(
            &xdg_path,
            "bloat_threshold = 0.2\nreformat_target_ratio = 0.1",
        )
        .unwrap();

        let config_dir = TempDir::new().unwrap();
        let dir_path = config_dir.path().to_path_buf();
        let dir_file = dir_path.join("pipeline.toml");
        std::fs::write(&dir_file, "bloat_threshold = 0.1").unwrap();

        let cli = Cli::parse_from(["stratum", "--config-dir", dir_path.to_str().unwrap(), "run"]);
        let (cfg, sources) = load_config(&cli, &env).unwrap();

        assert_eq!(cfg.bloat_threshold, Ratio::new_unchecked(0.1));
        // config_dir file did not set this field, and it overrides XDG entirely.
        assert_eq!(cfg.reformat_target_ratio, Ratio::new_unchecked(0.05));
        assert_eq!(sources.len(), 2);
        assert!(matches!(sources[0], ConfigSource::Embedded));
        assert!(matches!(sources[1], ConfigSource::ConfigDir { .. }));
    }

    #[test]
    fn config_dir_is_ignored_when_explicit_config_given() {
        let (_dir, config_home, env) = env_with_config_home();
        let xdg_path = config_home.join("stratum/pipeline.toml");
        std::fs::write(&xdg_path, "bloat_threshold = 0.2").unwrap();

        let config_dir = TempDir::new().unwrap();
        let dir_path = config_dir.path().to_path_buf();
        let dir_file = dir_path.join("pipeline.toml");
        std::fs::write(&dir_file, "bloat_threshold = 0.1").unwrap();

        let mut file = NamedTempFile::new().unwrap();
        writeln!(file, "bloat_threshold = 0.05").unwrap();

        let cli = Cli::parse_from([
            "stratum",
            "--config",
            file.path().to_str().unwrap(),
            "--config-dir",
            dir_path.to_str().unwrap(),
            "run",
        ]);
        let (cfg, sources) = load_config(&cli, &env).unwrap();

        assert_eq!(cfg.bloat_threshold, Ratio::new_unchecked(0.05));
        assert_eq!(sources.len(), 2);
        assert!(matches!(sources[1], ConfigSource::Explicit { .. }));
    }

    #[test]
    fn init_config_writes_to_config_dir_when_given() {
        let dir = TempDir::new().unwrap();
        let config_dir = dir.path().join("custom");
        let env = FakeEnv { config_home: None };

        let written = initialise_config(&env, Some(&config_dir), false).unwrap();
        assert_eq!(written, config_dir.join("pipeline.toml"));
        assert!(written.exists());

        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            let dir_mode = std::fs::metadata(&config_dir).unwrap().permissions().mode();
            assert_eq!(
                dir_mode & 0o777,
                0o700,
                "config directory must be owner-only"
            );
            let file_mode = std::fs::metadata(&written).unwrap().permissions().mode();
            assert_eq!(
                file_mode & 0o777,
                0o600,
                "config file must be owner-readable/writable only"
            );
        }
    }

    #[test]
    fn init_config_overwrites_existing_file_with_force() {
        let dir = TempDir::new().unwrap();
        let config_dir = dir.path().join("custom");
        let env = FakeEnv { config_home: None };
        let path = config_dir.join("pipeline.toml");

        let first = initialise_config(&env, Some(&config_dir), false).unwrap();
        assert_eq!(first, path);
        std::fs::write(&path, "bloat_threshold = 0.99").unwrap();

        let second = initialise_config(&env, Some(&config_dir), true).unwrap();
        assert_eq!(second, path);
        let contents = std::fs::read_to_string(&path).unwrap();
        assert!(
            !contents.contains("0.99"),
            "force=true should overwrite existing contents"
        );
    }

    #[test]
    fn init_config_without_force_does_not_create_directory() {
        let dir = TempDir::new().unwrap();
        let config_dir = dir.path().join("custom");
        let env = FakeEnv { config_home: None };
        let path = config_dir.join("pipeline.toml");

        std::fs::create_dir_all(&config_dir).unwrap();
        std::fs::write(&path, "bloat_threshold = 0.1").unwrap();

        let result = initialise_config(&env, Some(&config_dir), false);
        assert!(result.is_err());
        // The directory already existed, so we should not have removed it.
        assert!(config_dir.exists());
    }

    #[cfg(unix)]
    #[test]
    fn init_config_preserves_existing_directory_permissions() {
        use std::os::unix::fs::PermissionsExt;

        let dir = TempDir::new().unwrap();
        let config_dir = dir.path().join("custom");
        let env = FakeEnv { config_home: None };

        std::fs::create_dir_all(&config_dir).unwrap();
        let original_perms = std::fs::Permissions::from_mode(0o755);
        std::fs::set_permissions(&config_dir, original_perms).unwrap();

        let result = initialise_config(&env, Some(&config_dir), false);
        assert!(result.is_ok());

        let mode = std::fs::metadata(&config_dir).unwrap().permissions().mode() & 0o777;
        assert_eq!(
            mode, 0o755,
            "existing directory permissions must not be altered"
        );
    }
}
