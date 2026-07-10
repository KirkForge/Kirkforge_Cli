use crate::cli::Cli;
use kirkstratum_core::mode::{Mode, DEFAULT_MODE};

/// Resolve the effective mode.
///
/// Precedence (highest to lowest):
/// 1. Subcommand-specific mode argument.
/// 2. Global `--mode` flag (also populated from `STRATUM_MODE` by clap).
/// 3. Hard-coded default (`Mode::Full`).
#[must_use]
pub const fn resolve_mode_with_override(cli: &Cli, subcommand_mode: Option<Mode>) -> Mode {
    match subcommand_mode {
        Some(mode) => mode,
        None => match cli.mode {
            Some(mode) => mode,
            None => DEFAULT_MODE,
        },
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    const fn cli_with_mode(mode: Option<Mode>) -> Cli {
        Cli {
            config: None,
            config_dir: None,
            token_budget: None,
            json: false,
            max_input_size: None,
            mode,
            verbose: 0,
            quiet: 0,
            dry_run: false,
            command: crate::cli::Command::Run,
        }
    }

    #[test]
    fn subcommand_mode_wins_over_global_flag() {
        let cli = cli_with_mode(Some(Mode::Full));
        let mode = resolve_mode_with_override(&cli, Some(Mode::Off));
        assert_eq!(mode, Mode::Off);
    }

    #[test]
    fn global_mode_used_when_no_subcommand_mode() {
        let cli = cli_with_mode(Some(Mode::Lite));
        let mode = resolve_mode_with_override(&cli, None);
        assert_eq!(mode, Mode::Lite);
    }

    #[test]
    fn default_mode_used_when_nothing_set() {
        let cli = cli_with_mode(None);
        let mode = resolve_mode_with_override(&cli, None);
        assert_eq!(mode, DEFAULT_MODE);
    }
}
