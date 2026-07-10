//! Standalone CLI for KirkForge plugins.
//!
//! Commands:
//!   - `check <dir>`  Validate a plugin manifest.
//!   - `list`          List installed plugins.
//!   - `init <name>`  Scaffold a new plugin directory.

use anyhow::Context;
use clap::{Parser, Subcommand};
use kirkforge_plugin_host::{PluginRegistry, TrustPolicy};
use std::path::PathBuf;

#[derive(Parser)]
#[command(name = "kirkforge-plugin")]
#[command(about = "Validate, list, and scaffold KirkForge plugins")]
struct Cli {
    #[command(subcommand)]
    command: Command,
}

#[derive(Subcommand)]
enum Command {
    /// Validate a plugin manifest.
    Check {
        /// Path to the plugin directory.
        path: PathBuf,
    },
    /// List installed plugins.
    List {
        /// Plugins directory (default: ~/.local/share/kirkforge/plugins).
        #[arg(short, long)]
        dir: Option<PathBuf>,
    },
    /// Scaffold a new plugin directory.
    Init {
        /// Name of the new plugin.
        name: String,
        /// Output directory (default: current directory).
        #[arg(short, long)]
        output: Option<PathBuf>,
    },
}

fn main() -> anyhow::Result<()> {
    let cli = Cli::parse();

    match cli.command {
        Command::Check { path } => {
            kirkforge_plugin::LoadedPlugin::load(
                &path
                    .canonicalize()
                    .with_context(|| format!("cannot resolve {}", path.display()))?,
            )
            .with_context(|| format!("{} is not a valid plugin", path.display()))?;
            println!("✅ {} is a valid KirkForge plugin", path.display());
        }
        Command::List { dir } => {
            let dir = dir.unwrap_or_else(default_plugins_dir);
            let mut reg = PluginRegistry::new();
            let warnings = reg
                .load_from_dir(
                    &dir,
                    TrustPolicy::up_to(kirkforge_plugin::TrustTier::Unsafe),
                )
                .with_context(|| format!("cannot load plugins from {}", dir.display()))?;
            let active = reg.active_plugins();
            if active.is_empty() {
                println!("No plugins found in {}", dir.display());
            } else {
                println!("Plugins in {}:", dir.display());
                for p in active {
                    println!(
                        "  {} v{} — [{}] {}",
                        p.plugin.manifest.name,
                        p.plugin.manifest.version,
                        p.effective_trust,
                        p.plugin.manifest.description
                    );
                }
            }
            for w in warnings {
                eprintln!("⚠️  {w}");
            }
        }
        Command::Init { name, output } => {
            let out = output.unwrap_or_else(|| std::env::current_dir().unwrap_or_default());
            let root = out.join(&name);
            std::fs::create_dir_all(&root)?;
            std::fs::write(
                root.join("kirkforge.toml"),
                format!(
                    r#"name = "{name}"
version = "0.1.0"
description = "A KirkForge plugin"
trust = "read-only"

# Example skill:
# [[capabilities]]
# type = "skill"
# trigger = "/{name}"
# prompt = "Run the {name} plugin on: {{{{args}}}}"
"#
                ),
            )?;
            println!("✅ Scaffolded plugin at {}", root.display());
        }
    }

    Ok(())
}

fn default_plugins_dir() -> PathBuf {
    directories::ProjectDirs::from("", "", "kirkforge")
        .map(|d| d.data_dir().join("plugins"))
        .unwrap_or_else(|| PathBuf::from("plugins"))
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::process::Command;

    fn bin_path() -> PathBuf {
        std::env::var_os("CARGO_BIN_EXE_kirkforge-plugin")
            .map(PathBuf::from)
            .unwrap_or_else(|| {
                let mut path = PathBuf::from(env!("CARGO_MANIFEST_DIR"));
                path.pop();
                path.pop();
                path.join("target").join("debug").join("kirkforge-plugin")
            })
    }

    fn run<I, S>(args: I) -> std::process::Output
    where
        I: IntoIterator<Item = S>,
        S: AsRef<std::ffi::OsStr>,
    {
        Command::new(bin_path())
            .args(args)
            .output()
            .expect("failed to run kirkforge-plugin binary")
    }

    #[test]
    fn init_scaffolds_valid_plugin() {
        let tmp = tempfile::tempdir().unwrap();
        let name = "my-test-plugin";
        let out = run(["init", name, "--output", tmp.path().to_str().unwrap()]);
        assert!(
            out.status.success(),
            "stdout: {}, stderr: {}",
            String::from_utf8_lossy(&out.stdout),
            String::from_utf8_lossy(&out.stderr)
        );

        let plugin_dir = tmp.path().join(name);
        assert!(plugin_dir.join("kirkforge.toml").exists());

        let check = run(["check", plugin_dir.to_str().unwrap()]);
        assert!(
            check.status.success(),
            "check failed: stdout: {}, stderr: {}",
            String::from_utf8_lossy(&check.stdout),
            String::from_utf8_lossy(&check.stderr)
        );
    }

    #[test]
    fn check_rejects_invalid_plugin() {
        let tmp = tempfile::tempdir().unwrap();
        let out = run(["check", tmp.path().to_str().unwrap()]);
        assert!(!out.status.success());
    }

    #[test]
    fn list_shows_active_plugins_and_warnings() {
        let tmp = tempfile::tempdir().unwrap();
        let plugins = tmp.path().join("plugins");

        // Valid read-only plugin.
        let valid = plugins.join("valid");
        std::fs::create_dir_all(&valid).unwrap();
        std::fs::write(
            valid.join("kirkforge.toml"),
            r#"
name = "valid"
version = "0.1.0"
description = "valid plugin"
trust = "read-only"

[[capabilities]]
type = "skill"
trigger = "/valid"
prompt = "hello"
"#,
        )
        .unwrap();

        // Unsafe plugin that should be rejected by default list policy.
        let risky = plugins.join("risky");
        std::fs::create_dir_all(&risky).unwrap();
        std::fs::write(
            risky.join("kirkforge.toml"),
            r#"
name = "risky"
version = "0.1.0"
description = "risky plugin"
trust = "unsafe"
"#,
        )
        .unwrap();

        let out = run(["list", "--dir", plugins.to_str().unwrap()]);
        assert!(
            out.status.success(),
            "stdout: {}, stderr: {}",
            String::from_utf8_lossy(&out.stdout),
            String::from_utf8_lossy(&out.stderr)
        );

        let stdout = String::from_utf8_lossy(&out.stdout);
        assert!(stdout.contains("valid"), "missing valid plugin: {stdout}");
        assert!(stdout.contains("risky"), "missing risky warning: {stdout}");
    }
}
