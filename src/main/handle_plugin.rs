// `kf-code plugin <subcommand>` dispatch (WO 11.0, ADR-056).
// Extracted from the binary root — pure move, no behaviour change.

use kf_code::cli::PluginCommand;

/// Dispatch the `kf-code plugin` CLI subcommand (WO 11.0, ADR-056).
///
/// Loads the shared config once, runs the requested op via the shared
/// `plugin_ops` layer (the same layer the TUI `/plugins` commands will
/// migrate to), prints the result, and persists any config mutation.
pub(super) fn handle_plugin_command(command: PluginCommand) -> anyhow::Result<()> {
    use kf_code::session::plugin_ops as ops;
    let mut cfg = kf_code::session::config::load_or_create_config();
    match command {
        PluginCommand::List => {
            println!("{}", ops::list(&cfg));
        }
        PluginCommand::Enable { name } => {
            println!("{}", ops::enable(&mut cfg, &name)?);
        }
        PluginCommand::Disable { name } => {
            println!("{}", ops::disable(&mut cfg, &name)?);
        }
        PluginCommand::Toggle { name } => {
            println!("{}", ops::toggle(&mut cfg, &name)?);
        }
        PluginCommand::Validate { path } => {
            println!("{}", ops::validate(&path)?);
        }
        PluginCommand::Reload => {
            // The CLI has no live registry; reload == re-load and report.
            let (registry, warnings) = kf_code::session::plugin_tools::load_plugin_registry(&cfg)?;
            println!("Reloaded plugins: {} active.", registry.active_count());
            if !warnings.is_empty() {
                println!("Warnings:");
                for w in &warnings {
                    println!("  - {w}");
                }
            }
        }
        PluginCommand::Sources => {
            println!("{}", ops::sources(&cfg));
        }
        PluginCommand::Add { name, path } => {
            println!(
                "{}",
                ops::add_source(&mut cfg, &name, &path.to_string_lossy())?
            );
        }
        PluginCommand::Remove { name } => {
            println!("{}", ops::remove_source(&mut cfg, &name)?);
        }
        PluginCommand::Doctor => {
            println!("{}", ops::doctor(&cfg));
        }
        PluginCommand::Init { name, path } => {
            let plugin_dir = ops::init(&name, path.as_deref())?;
            println!(
                "Plugin scaffolded at {}. Edit `kf-code.toml`, then run \
                 `kf-code plugin enable {name}` (or `/plugins enable {name}` \
                 in the TUI) to activate.",
                plugin_dir.display()
            );
        }
    }
    Ok(())
}
