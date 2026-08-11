//! Central color palette for the TUI.
//!
//! `Theme` carries every color the markdown renderer, search highlighter,
//! table renderer, and budget indicator use. The active instance lives on
//! `UiState::theme` (`src/tui/app.rs`); render functions in
//! `src/tui/rendering/` take a `&Theme` and look up roles by field name.
//!
//! Built-in themes:
//!   - [`Theme::default`] — current colors (back-compat baseline)
//!   - [`Theme::dark`]    — high-contrast dark background
//!   - [`Theme::light`]   — readable on white / light terminals
//!   - [`Theme::monokai`] — popular warm palette
//!
//! Use [`Theme::from_name`] to resolve a config string (`"default"` etc.)
//! to a `Theme`. Unknown names fall back to `default`.
//!
// ponytail: 4 built-in themes; custom user palettes (TOML-loaded) are the
// upgrade path if requested. Adding a 5th built-in is a 1-line constructor.

use ratatui::style::Color;

/// The color palette used by every renderer under `src/tui/rendering/`.
///
/// Field set was built by grepping every `Color::*` literal in
/// `src/tui/rendering/` (mod.rs, table.rs, format.rs) and naming each
/// distinct visual role. Do not add fields that are not referenced by a
/// rendering call site — every field here earns its place.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Theme {
    // ── Markdown: code blocks ────────────────────────────────────
    /// Fg color of code-block body text.
    pub code_block_fg: Color,
    /// Background color of code-block body.
    pub code_block_bg: Color,
    /// Fg color of the `▌` / `▕` border chars around code blocks.
    pub code_block_border: Color,
    /// Fg color of the language-label header line on fenced code blocks.
    pub code_block_header: Color,
    /// Fg color of inline `code` spans.
    pub inline_code: Color,

    // ── Markdown: prose ──────────────────────────────────────────
    /// Fg color of blockquote body text.
    pub blockquote_fg: Color,
    /// Fg color of H1 headings (other headings use the default fg).
    pub heading: Color,
    /// Fg color of hyperlink text.
    pub link: Color,
    /// Fg color of image-text fallback spans.
    pub image: Color,
    /// Fg color of `[x]` / `[ ]` task-list markers.
    pub tasklist_marker: Color,

    // ── Search highlight ─────────────────────────────────────────
    /// Fg color of a search-match highlight (drawn over the match).
    pub search_fg: Color,
    /// Bg color of a search-match highlight.
    pub search_bg: Color,

    // ── Table grid ───────────────────────────────────────────────
    /// Fg color of plain table cell text.
    pub table_base: Color,
    /// Fg color of the `|---|---|` separator line under the header row.
    pub table_separator: Color,

    // ── Budget indicator ─────────────────────────────────────────
    /// Used when no model / no budget is known (neutral cue).
    pub budget_neutral: Color,
    /// Token pressure < 50% (comfortable).
    pub budget_comfortable: Color,
    /// Token pressure 50–80% (consider `/compact`).
    pub budget_tight: Color,
    /// Token pressure 80–95% (compact now).
    pub budget_high: Color,
    /// Token pressure ≥ 95% (the cliff).
    pub budget_cliff: Color,
}

impl Theme {
    /// Current colors — exact back-compat baseline. Every prior
    /// `Color::*` literal in `src/tui/rendering/` is preserved here.
    fn default_colors() -> Self {
        Self {
            code_block_fg: Color::Rgb(180, 180, 140),
            code_block_bg: Color::Rgb(45, 45, 40),
            code_block_border: Color::DarkGray,
            code_block_header: Color::Gray,
            inline_code: Color::Rgb(230, 160, 50),
            blockquote_fg: Color::Rgb(180, 180, 180),
            heading: Color::White,
            link: Color::Cyan,
            image: Color::Magenta,
            tasklist_marker: Color::Yellow,
            search_fg: Color::Black,
            search_bg: Color::Yellow,
            table_base: Color::White,
            table_separator: Color::DarkGray,
            budget_neutral: Color::DarkGray,
            budget_comfortable: Color::Green,
            budget_tight: Color::Yellow,
            budget_high: Color::Red,
            budget_cliff: Color::Rgb(255, 100, 100),
        }
    }

    /// High-contrast dark theme. Brighter code/block colors so the
    /// warm-on-warm default code block reads on true-black terminals.
    pub fn dark() -> Self {
        Self {
            code_block_fg: Color::Rgb(220, 220, 180),
            code_block_bg: Color::Rgb(30, 30, 30),
            code_block_border: Color::Gray,
            code_block_header: Color::White,
            inline_code: Color::Rgb(255, 176, 80),
            blockquote_fg: Color::Rgb(200, 200, 200),
            heading: Color::White,
            link: Color::Cyan,
            image: Color::Magenta,
            tasklist_marker: Color::Yellow,
            search_fg: Color::Black,
            search_bg: Color::Yellow,
            table_base: Color::White,
            table_separator: Color::Gray,
            budget_neutral: Color::Gray,
            budget_comfortable: Color::Green,
            budget_tight: Color::Yellow,
            budget_high: Color::Red,
            budget_cliff: Color::Rgb(255, 120, 120),
        }
    }

    /// Light-theme palette for white / pale terminal backgrounds.
    /// Swaps low-luminance colors (Black, Cyan, Yellow) for higher-
    /// luminance alternatives so highlights remain visible on white.
    pub fn light() -> Self {
        Self {
            code_block_fg: Color::Rgb(60, 60, 40),
            code_block_bg: Color::Rgb(235, 235, 225),
            code_block_border: Color::DarkGray,
            code_block_header: Color::Black,
            inline_code: Color::Rgb(180, 95, 0),
            blockquote_fg: Color::DarkGray,
            heading: Color::Black,
            link: Color::Blue,
            image: Color::Magenta,
            // Standard ANSI Yellow renders as olive / amber on light
            // backgrounds — visible without needing a custom Rgb.
            tasklist_marker: Color::Yellow,
            // Black text on a saturated amber background reads on white.
            search_fg: Color::Black,
            search_bg: Color::Rgb(255, 205, 0),
            table_base: Color::Black,
            table_separator: Color::DarkGray,
            budget_neutral: Color::DarkGray,
            budget_comfortable: Color::Green,
            budget_tight: Color::Rgb(200, 130, 0),
            budget_high: Color::Red,
            budget_cliff: Color::Rgb(220, 0, 0),
        }
    }

    /// Monokai-inspired warm palette. Translates the canonical Monokai
    /// hex values into the closest `Color::*` variants (the renderer
    /// already supports `Color::Rgb`, so exact hex is preserved).
    pub fn monokai() -> Self {
        Self {
            // Monokai background is #272822; comments #75715E.
            code_block_fg: Color::Rgb(248, 248, 242), // foreground
            code_block_bg: Color::Rgb(39, 40, 34),    // background
            code_block_border: Color::Rgb(117, 113, 94), // comment
            code_block_header: Color::Rgb(166, 226, 46), // green (type/class)
            inline_code: Color::Rgb(253, 151, 31),    // orange (numbers)
            blockquote_fg: Color::Rgb(147, 161, 161), // soft gray
            heading: Color::Rgb(249, 38, 114),        // pink (keywords)
            link: Color::Rgb(102, 217, 239),          // cyan
            image: Color::Rgb(249, 38, 114),          // pink
            tasklist_marker: Color::Rgb(230, 219, 116), // yellow
            search_fg: Color::Rgb(39, 40, 34),
            search_bg: Color::Rgb(230, 219, 116),
            table_base: Color::Rgb(248, 248, 242),
            table_separator: Color::Rgb(117, 113, 94),
            budget_neutral: Color::Rgb(117, 113, 94),
            budget_comfortable: Color::Rgb(166, 226, 46), // green
            budget_tight: Color::Rgb(230, 219, 116),      // yellow
            budget_high: Color::Rgb(249, 38, 114),        // pink
            budget_cliff: Color::Rgb(255, 90, 90),
        }
    }

    /// Resolve a config-string theme name to a `Theme`.
    /// Unknown names fall back to `default`.
    pub fn from_name(name: &str) -> Self {
        match name.trim() {
            "dark" => Self::dark(),
            "light" => Self::light(),
            "monokai" => Self::monokai(),
            _ => Self::default(),
        }
    }

    /// Names of the built-in themes, in cycle order. Used by the
    /// `/theme` slash command for tab-completion-style help text.
    pub const BUILTIN_NAMES: &'static [&'static str] = &["default", "dark", "light", "monokai"];

    /// Next theme name in the default→dark→light→monokai→default cycle.
    pub fn next_name(current: &str) -> &'static str {
        let cur = current.trim();
        let idx = Self::BUILTIN_NAMES
            .iter()
            .position(|n| *n == cur)
            .unwrap_or(0);
        let next = (idx + 1) % Self::BUILTIN_NAMES.len();
        Self::BUILTIN_NAMES[next]
    }
}

impl Default for Theme {
    fn default() -> Self {
        Self::default_colors()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn default_preserves_prior_code_block_colors() {
        let t = Theme::default();
        // Back-compat pin: the prior literals were Rgb(180,180,140)
        // fg on Rgb(45,45,40) bg, with DarkGray border.
        assert_eq!(t.code_block_fg, Color::Rgb(180, 180, 140));
        assert_eq!(t.code_block_bg, Color::Rgb(45, 45, 40));
        assert_eq!(t.code_block_border, Color::DarkGray);
    }

    #[test]
    fn default_preserves_budget_threshold_colors() {
        let t = Theme::default();
        assert_eq!(t.budget_comfortable, Color::Green);
        assert_eq!(t.budget_tight, Color::Yellow);
        assert_eq!(t.budget_high, Color::Red);
        assert_eq!(t.budget_cliff, Color::Rgb(255, 100, 100));
    }

    #[test]
    fn from_name_resolves_builtins() {
        assert_eq!(Theme::from_name("default"), Theme::default());
        assert_eq!(Theme::from_name("dark"), Theme::dark());
        assert_eq!(Theme::from_name("light"), Theme::light());
        assert_eq!(Theme::from_name("monokai"), Theme::monokai());
    }

    #[test]
    fn from_name_unknown_falls_back_to_default() {
        assert_eq!(Theme::from_name("nord"), Theme::default());
        assert_eq!(Theme::from_name(""), Theme::default());
    }

    #[test]
    fn next_name_cycles_in_order() {
        assert_eq!(Theme::next_name("default"), "dark");
        assert_eq!(Theme::next_name("dark"), "light");
        assert_eq!(Theme::next_name("light"), "monokai");
        assert_eq!(Theme::next_name("monokai"), "default");
    }

    #[test]
    fn next_name_unknown_starts_cycle() {
        assert_eq!(Theme::next_name("nord"), "dark");
    }

    #[test]
    fn light_theme_uses_readable_high_luminance_colors() {
        // The whole point of /theme light: black-on-yellow search
        // highlight is invisible on white, so light() must use a
        // saturated amber background that reads on white.
        let t = Theme::light();
        assert_eq!(t.search_fg, Color::Black);
        assert_eq!(t.search_bg, Color::Rgb(255, 205, 0));
    }

    #[test]
    fn builtin_names_lists_all_four_in_cycle_order() {
        assert_eq!(
            Theme::BUILTIN_NAMES,
            &["default", "dark", "light", "monokai"]
        );
    }
}
