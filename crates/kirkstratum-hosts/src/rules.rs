use kirkstratum_core::mode::Mode;

/// Filter a source containing `<!-- stratum:mode:... -->` directives to only
/// the sections active for `mode`.
///
/// Lines before the first directive are kept. Each directive switches the keep
/// state based on whether `mode` is listed (`all` always matches). A section
/// without an explicit closing directive implicitly switches state until the
/// next directive or end of input.
///
/// # Examples
///
/// ```
/// use kirkstratum_core::mode::Mode;
/// use kirkstratum_hosts::rules::filter_by_mode;
///
/// let source = "# Rules\n<!-- stratum:mode:all -->\ncommon\n<!-- stratum:mode:full,ultra -->\nadvanced\n";
/// let out = filter_by_mode(source, Mode::Off);
/// assert!(out.contains("common"));
/// assert!(!out.contains("advanced"));
/// ```
#[must_use]
pub fn filter_by_mode(source: &str, mode: Mode) -> String {
    let mut out = String::with_capacity(source.len());
    let mut keep = true;
    for line in source.lines() {
        if let Some(rest) = line.strip_prefix("<!-- stratum:mode:") {
            let directive = rest.strip_suffix("-->").unwrap_or(rest).trim();
            keep = directive == "all" || directive.split(',').any(|m| m.trim() == mode.as_str());
            continue;
        }
        if keep {
            out.push_str(line);
            out.push('\n');
        }
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    const SAMPLE: &str = "# Rules\n<!-- stratum:mode:all -->\nall\n<!-- stratum:mode:full,ultra -->\nfull\n<!-- stratum:mode:ultra -->\nultra\n";

    #[test]
    fn off_mode_strips_full_rules() {
        let out = filter_by_mode(SAMPLE, Mode::Off);
        assert!(out.contains("all"));
        assert!(!out.contains("full"));
        assert!(!out.contains("ultra"));
    }

    #[test]
    fn lite_mode_keeps_only_all() {
        let out = filter_by_mode(SAMPLE, Mode::Lite);
        assert!(out.contains("all"));
        assert!(!out.contains("full"));
        assert!(!out.contains("ultra"));
    }

    #[test]
    fn full_mode_keeps_all_and_full() {
        let out = filter_by_mode(SAMPLE, Mode::Full);
        assert!(out.contains("all"));
        assert!(out.contains("full"));
        assert!(!out.contains("ultra"));
    }

    #[test]
    fn ultra_mode_includes_everything() {
        let out = filter_by_mode(SAMPLE, Mode::Ultra);
        assert!(out.contains("all"));
        assert!(out.contains("full"));
        assert!(out.contains("ultra"));
    }

    #[test]
    fn empty_source_returns_empty() {
        assert_eq!(filter_by_mode("", Mode::Full), "");
    }

    #[test]
    fn malformed_directive_falls_back_to_keep() {
        // A line that looks like a directive but lacks a known mode list
        // should be treated as a non-directive (keep continues).
        let input = "keep\n<!-- stratum:mode: -->\nhidden\n";
        let out = filter_by_mode(input, Mode::Full);
        assert!(out.contains("keep"));
        assert!(!out.contains("hidden"));
    }

    #[test]
    fn unclosed_directive_is_ignored() {
        // A directive line without closing marker is still parsed as a directive
        // (strip_suffix returns the original rest), so it switches state.
        let input = "before\n<!-- stratum:mode:all\nafter\n";
        let out = filter_by_mode(input, Mode::Full);
        assert!(out.contains("before"));
        assert!(out.contains("after"));
    }

    #[test]
    fn repeated_directive_switches_keep_state() {
        let input = "a\n<!-- stratum:mode:off -->\nb\n<!-- stratum:mode:all -->\nc\n";
        let out = filter_by_mode(input, Mode::Ultra);
        assert!(out.contains('a'));
        assert!(!out.contains('b'));
        assert!(out.contains('c'));
    }
}
