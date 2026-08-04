use crate::stdout::write_stdout;
use anyhow::Context;

/// Emit either machine-readable JSON or human-readable text to stdout.
#[must_use = "output emission errors should be handled"]
pub fn emit_json_or_human(
    json: bool,
    human: &str,
    machine: &serde_json::Value,
) -> anyhow::Result<()> {
    if json {
        write_stdout(&format!("{}\n", serde_json::to_string_pretty(machine)?))
            .with_context(|| "failed to write JSON to stdout")?;
    } else {
        write_stdout(human).with_context(|| "failed to write output to stdout")?;
    }
    Ok(())
}
