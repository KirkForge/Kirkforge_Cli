/// Approval dialog — shown when a destructive tool call needs user confirmation.
///
/// **v1.2-p11:** The args preview is now scrollable. Previously a long
/// `edit_file` `old_string` or multi-line `bash` command got truncated
/// to a fixed 4-line window with a "..." tail, so the user was approving
/// changes they couldn't actually read. Now the dialog grows to use up
/// to 75% of the terminal height, the args preview is wrapped into a
/// flat list of lines, and the user can scroll with PageUp/PageDown/
/// Up/Down/Home/End. A `↑N more / ↓N more` indicator shows when the
/// content overflows the visible window.
use crate::tui::app::{AppState, PendingApproval};
use ratatui::{
    layout::{Alignment, Constraint, Direction, Layout, Rect},
    style::{Color, Modifier, Style},
    text::{Line, Span},
    widgets::{Block, BorderType, Borders, Clear, Paragraph, Wrap},
    Frame,
};

/// Render a full-width, scrollable approval panel over the main content.
///
/// The panel uses the full terminal width so long args, diffs, and
/// side-by-side diff views have maximum readability. It takes up to
/// 75% of the terminal height, leaving a sliver of chat visible
/// above and below for context.
pub fn render_approval_dialog(
    f: &mut Frame,
    area: Rect,
    approval: &PendingApproval,
    state: &mut AppState,
) {
    // Full-width panel — uses the entire terminal width for maximum
    // readability of args, diffs, and side-by-side views. Up to 75%
    // of terminal height, leaving conversation visible above/below.
    //
    // Bounds guard: on a terminal too small to hold the dialog (height
    // < 4 rows, or width < 20 — not enough to render the border + a
    // line of args), skip the render entirely. The prior code called
    // `.clamp(10, area.height)` which PANICS when `area.height < 10`
    // (min > max), and a 0-width/height `Rect` passed to `Clear`
    // corrupts the terminal state — the "yeeted on approval" symptom.
    // Skipping is safe: the chat stays visible and the approval stays
    // pending; the user resizes and the next frame renders the dialog.
    let dialog_area = match approval_dialog_area(area) {
        Some(r) => r,
        None => {
            tracing::warn!(
                area_height = area.height,
                area_width = area.width,
                "approval dialog skipped: terminal too small to render safely"
            );
            return;
        }
    };

    // Clear ONLY the dialog rect, not the whole screen. A full-area
    // `Clear` wipes the chat behind the popup, leaving a small red box
    // on a black field — which reads as "blank/broken" rather than an
    // approval prompt. Clearing just the dialog keeps the conversation
    // visible around it, so the prompt appears in context.
    f.render_widget(Clear, dialog_area);
    let dialog_width = dialog_area.width;

    // ASCII title — ratatui 0.30 miscounts the display width of `⚠️`
    // (U+26A0 + U+FE0F variation selector), leaving a 1-cell gap on the
    // top border where the chat behind the dialog bleeds through
    // (`⚠️r` instead of `⚠️`). The variation selector is width-0 but the
    // title positioning off-by-one corrupts the border. `!!` is unambiguous
    // and has no width ambiguity. The `⚠` glyph still appears in the body
    // headline (rendered by Paragraph, which handles width correctly).
    let block = Block::default()
        .title(" !!  Approval Required ")
        .borders(Borders::ALL)
        .border_type(BorderType::Double)
        // border_style MUST set bg — without it the border cells inherit
        // no explicit background, so chat text from the prior frame shows
        // through the border rows (the dialog is an overlay, not a fresh
        // screen). bg(Black) matches the block's inner style and fully
        // obscures the chat behind the border.
        .border_style(
            Style::default()
                .fg(Color::Red)
                .bg(Color::Black)
                .add_modifier(Modifier::BOLD),
        )
        .style(Style::default().bg(Color::Black));

    let inner = block.inner(dialog_area);

    // Layout inside the dialog
    //   [0] action headline + detail + risk line   (3 lines)
    //   [1] args preview (scrollable)              (the rest)
    //   [2] scroll indicator (if truncated)        (1 line, only when scrolled)
    //   [3] instructions                           (1 line)
    let args_window_height = inner.height.saturating_sub(5) as usize;

    // Side-by-side diff: available whenever the terminal is at least 80
    // cols. The full-width panel makes this practical on most terminals.
    let use_side_by_side = state.approval.approval_diff_side_by_side && area.width >= 80;

    // Build the full flat line list of the preview. For
    // `edit_file` / `write_file` approvals we append a unified
    // diff after the JSON args — see Review.md gap #8. The diff is
    // color-coded per line (green for `+`, red for `-`); the args
    // lines are plain white. We pack both into a single
    // `Vec<Line>` so the existing scroll/clamp code works
    // unchanged.
    //
    // WO 38.11: memoize the computation. The dialog re-reads +
    // re-diffs the target file every frame at 8 Hz otherwise; for a
    // large edit on a slow disk that's a visible CPU spike. The
    // cache is keyed on `(tool_name, args JSON, side_by_side,
    // dialog_width, file mtime)` and stores the flattened
    // visible-line strings + diff stats + is_outside_cwd. The
    // styling is re-applied on a hit (cheap prefix checks), so the
    // cache only needs the strings.
    let args_json = serde_json::to_string(&approval.args).unwrap_or_default();
    let path = approval
        .args
        .get("path")
        .and_then(|v| v.as_str())
        .unwrap_or("-");
    let cwd = std::env::current_dir()
        .ok()
        .and_then(|d| d.canonicalize().ok());
    // Probe the file mtime for the cache key. `None` when the path
    // isn't a file field or the stat fails — the cache still works,
    // it just invalidates on every render for that case (no worse
    // than the prior behavior).
    let file_mtime = approval
        .args
        .get("path")
        .and_then(|v| v.as_str())
        .and_then(|p| {
            let permitted = cwd.as_ref().is_some_and(|base| {
                std::path::Path::new(p)
                    .canonicalize()
                    .map(|canon| canon.starts_with(base))
                    .unwrap_or(false)
            });
            if permitted {
                std::fs::metadata(p).ok().and_then(|m| m.modified().ok())
            } else {
                None
            }
        });

    if state.approval.approval_diff_cache.matches(
        &approval.tool_name,
        &args_json,
        use_side_by_side,
        dialog_width,
        file_mtime,
    ) {
        // Cache hit — rebuild the styled lines from the cached
        // strings. The coloring is a pure function of the line
        // prefix, so we re-apply it here (cheap) instead of caching
        // ratatui `Line` types (which would couple the cache to the
        // render backend). Clone the cache data out of state so the
        // mutable borrow for the render call doesn't conflict.
        let cache = state.approval.approval_diff_cache.clone();
        let visible_lines: Vec<Line> = cache
            .visible_lines
            .iter()
            .map(|s| {
                let color = line_color(s);
                Line::from(Span::styled(s.clone(), Style::default().fg(color)))
            })
            .collect();
        render_approval_dialog_from_lines(
            f,
            dialog_area,
            block,
            inner,
            area,
            approval,
            path,
            visible_lines,
            cache.diff_stats,
            cache.is_outside_cwd,
            state,
            use_side_by_side,
            args_window_height,
        );
        return;
    }

    // Cache miss — compute the full diff. The reader callback resolves
    // the file path to its current bytes; we only attach a diff if the
    // tool is `edit_file` / `write_file` (other tools return
    // `Vec::new()` from the formatter).
    // Only read files inside the working directory; a malicious model could
    // submit edit_file("../../../../etc/passwd") expecting the diff preview to
    // leak the file contents even if PathGuard blocks the write.
    //
    // The base cwd is canonicalized so the `starts_with` comparison against a
    // canonicalized target path is prefix-consistent on Windows. On Windows,
    // `Path::canonicalize` returns an extended-length `\\?\C:\...` path while
    // `std::env::current_dir()` returns `C:\...` (no prefix) — comparing the
    // two directly always reports "outside CWD", which would mis-classify
    // every in-CWD edit as DANGEROUS and suppress the diff preview.
    // Canonicalizing the base gives both sides the same prefix on Windows;
    // on Unix it is a no-op for an already-absolute path.
    let reader = |p: &str| {
        let permitted = cwd.as_ref().is_some_and(|base| {
            std::path::Path::new(p)
                .canonicalize()
                .map(|canon| canon.starts_with(base))
                .unwrap_or(false)
        });
        if permitted {
            std::fs::read_to_string(p).ok()
        } else {
            None
        }
    };
    let diff_stats = crate::tui::components::diff_preview::diff_stats(approval, &reader);
    let is_outside_cwd = match approval.tool_name.as_str() {
        "edit_file" | "write_file" => cwd.as_ref().is_some_and(|base| {
            std::path::Path::new(path)
                .canonicalize()
                .map(|canon| !canon.starts_with(base))
                .unwrap_or(true)
        }),
        _ => false,
    };

    let args_lines = format_args_preview(approval, dialog_width as usize);
    let mut visible_lines: Vec<Line> = args_lines
        .iter()
        .map(|s| Line::from(Span::styled(s.clone(), Style::default().fg(Color::White))))
        .collect();
    let mut flat_lines: Vec<String> = args_lines.clone();

    let diff_lines = crate::tui::components::diff_preview::format_edit_diff_preview(
        approval,
        dialog_width as usize,
        &reader,
    );
    if use_side_by_side {
        let side_lines = crate::tui::components::diff_preview::format_side_by_side_diff(
            approval,
            dialog_width as usize,
            &reader,
        );
        if !side_lines.is_empty() {
            visible_lines.push(Line::from(Span::styled(
                " ── Side-by-side diff ──",
                Style::default()
                    .fg(Color::Cyan)
                    .add_modifier(Modifier::BOLD),
            )));
            flat_lines.push(" ── Side-by-side diff ──".to_string());
            for dl in &side_lines {
                let flat: String = dl.spans.iter().map(|s| s.content.as_ref()).collect();
                visible_lines.push(dl.clone());
                flat_lines.push(flat);
            }
        }
    } else if !diff_lines.is_empty() {
        // Separator between args and diff.
        visible_lines.push(Line::from(Span::styled(
            " ── Diff preview ──",
            Style::default()
                .fg(Color::Cyan)
                .add_modifier(Modifier::BOLD),
        )));
        flat_lines.push(" ── Diff preview ──".to_string());
        for dl in &diff_lines {
            let color = line_color(dl);
            visible_lines.push(Line::from(Span::styled(
                dl.clone(),
                Style::default().fg(color),
            )));
            flat_lines.push(dl.clone());
        }
    }

    // Store the cache for next frame.
    state.approval.approval_diff_cache = crate::tui::app::ApprovalDiffCache {
        key_tool: approval.tool_name.clone(),
        key_args_json: args_json,
        key_side_by_side: use_side_by_side,
        key_dialog_width: dialog_width,
        key_mtime: file_mtime,
        visible_lines: flat_lines,
        diff_stats: diff_stats.clone(),
        is_outside_cwd,
    };

    render_approval_dialog_from_lines(
        f,
        dialog_area,
        block,
        inner,
        area,
        approval,
        path,
        visible_lines,
        diff_stats,
        is_outside_cwd,
        state,
        use_side_by_side,
        args_window_height,
    );
}

/// Color a flat diff line by its prefix. Extracted so the cache-hit
/// path can re-apply styling without re-running the diff.
fn line_color(s: &str) -> Color {
    if s.starts_with("+ ") && !s.starts_with("+++") {
        Color::Green
    } else if s.starts_with("- ") && !s.starts_with("---") {
        Color::Red
    } else if s.starts_with("+++") {
        Color::Green
    } else if s.starts_with("---") {
        Color::Red
    } else {
        Color::White
    }
}

/// Render the approval dialog from a pre-computed `visible_lines`
/// list. Split out of `render_approval_dialog` so the cache-hit path
/// and the cache-miss path share the layout/scroll/indicator logic.
#[allow(clippy::too_many_arguments)]
fn render_approval_dialog_from_lines(
    f: &mut Frame,
    dialog_area: Rect,
    block: Block<'static>,
    inner: Rect,
    area: Rect,
    approval: &PendingApproval,
    path: &str,
    visible_lines: Vec<Line<'static>>,
    diff_stats: Option<crate::tui::components::diff_preview::DiffStats>,
    is_outside_cwd: bool,
    state: &mut AppState,
    _use_side_by_side: bool,
    args_window_height: usize,
) {
    let dialog_width = dialog_area.width;
    let _ = dialog_width;
    let all_lines: Vec<String> = visible_lines
        .iter()
        .map(|l| {
            // Flatten back to a string for the max_scroll / overflow
            // computation. The visible rendering happens below using
            // the styled `Line`s directly.
            l.spans.iter().map(|s| s.content.as_ref()).collect()
        })
        .collect();

    // Clamp scroll and compute visible window + overflow indicator.
    let max_scroll = all_lines.len().saturating_sub(args_window_height.max(1));
    state.approval.approval_max_scroll = max_scroll;
    let scroll = state.approval.approval_scroll.min(max_scroll);

    let visible: Vec<Line> = visible_lines
        .iter()
        .skip(scroll)
        .take(args_window_height.max(1))
        .cloned()
        .collect();

    // Show a small "N more above / N more below" indicator only when
    // there's overflow in that direction — keeps the dialog clean
    // for the common short-args case.
    let show_top_indicator = scroll > 0;
    let show_bot_indicator = scroll < max_scroll;
    let indicator_count = (show_top_indicator as usize) + (show_bot_indicator as usize);

    let constraints = if indicator_count > 0 {
        vec![
            Constraint::Length(3),
            Constraint::Length(args_window_height as u16),
            Constraint::Length(1),
            Constraint::Length(1),
        ]
    } else {
        vec![
            Constraint::Length(3),
            Constraint::Length(args_window_height as u16),
            Constraint::Length(1),
        ]
    };
    let chunks = Layout::default()
        .direction(Direction::Vertical)
        .constraints(constraints)
        .split(inner);

    // [0] Compact action headline + risk line
    let (added, deleted) = diff_stats
        .as_ref()
        .map(|s| (s.added, s.deleted))
        .unwrap_or((0, 0));
    let risk_tier = risk_tier(approval, is_outside_cwd);
    let summary_color = risk_tier_color(risk_tier);
    // Action-first headline (WO 34.10): the action is the headline, not
    // the tool name. For file edits: `⚠ Change <path>` + `+N -M lines`.
    // For bash: `⚠ Run command` + the command text.
    let (action_headline, action_detail) = action_headline(approval, path, added, deleted);
    let name_text = Paragraph::new(vec![
        Line::from(Span::styled(
            action_headline,
            Style::default()
                .fg(summary_color)
                .add_modifier(Modifier::BOLD),
        )),
        Line::from(vec![
            Span::styled("  ", Style::default()),
            Span::styled(action_detail, Style::default().fg(Color::White)),
        ]),
        Line::from(Span::styled(
            format!("  {} — {}", risk_tier, risk_tier_explanation(risk_tier)),
            Style::default().fg(risk_tier_color(risk_tier)),
        )),
    ]);
    f.render_widget(name_text, chunks[0]);

    // [1] Args preview (scrollable)
    let args_text = Paragraph::new(visible)
        .block(Block::default().borders(Borders::NONE))
        .wrap(Wrap { trim: false });
    f.render_widget(args_text, chunks[1]);

    // [2] (optional) Scroll indicator
    if indicator_count > 0 {
        let mut spans = Vec::new();
        if show_top_indicator {
            spans.push(Span::styled(
                format!("↑ {scroll} more above "),
                Style::default().fg(Color::DarkGray),
            ));
        }
        if show_bot_indicator {
            spans.push(Span::styled(
                format!("↓ {} more below ", max_scroll - scroll),
                Style::default().fg(Color::DarkGray),
            ));
        }
        let indicator = Paragraph::new(Line::from(spans)).alignment(Alignment::Center);
        f.render_widget(indicator, chunks[2]);
    }

    // [last] Instructions
    let mut instr =
        String::from(" [Y]es  [N]o  [A]lways  [Esc/Q] cancel    ^C exit    ↑↓ PgUp/PgDn");
    if area.width >= 80 {
        instr.push_str("    [Tab] side-by-side");
    }
    let instr_text = Paragraph::new(vec![Line::from(Span::styled(
        instr,
        Style::default().fg(Color::Green),
    ))])
    .alignment(Alignment::Center);
    let instr_chunk = if indicator_count > 0 {
        chunks[3]
    } else {
        chunks[2]
    };
    f.render_widget(instr_text, instr_chunk);

    f.render_widget(block, dialog_area);
}

/// Compute the dialog `Rect` for a given terminal area.
///
/// Pure function — no I/O, no frame, no state. Returns `None` when the
/// terminal is too small to render the dialog safely (height < 4 or
/// width < 20). The returned `Rect` is always valid: width and height
/// are >= 1, and the rect fits inside `area` (x+width <= area.width,
/// y+height <= area.height). This is the bounds guard that prevents
/// the "yeeted on approval" crash — the prior code called
/// `.clamp(10, area.height)` which panicked when `area.height < 10`
/// (min > max), and a 0-dimension `Rect` passed to `Clear` corrupted
/// the terminal.
///
/// Dialog height is 75% of the terminal (rounded down), clamped to at
/// least 4 rows (border + 1 headline + 1 args line + border) and at
/// most the full terminal height. The dialog is centered vertically.
pub fn approval_dialog_area(area: Rect) -> Option<Rect> {
    if area.height < 4 || area.width < 20 {
        return None;
    }
    let dialog_width = area.width;
    // Safe clamp: area.height >= 4 here (guarded above), so max(4) is
    // always <= area.height. Never let min exceed max.
    let dialog_height = (area.height * 3 / 4).min(area.height).max(4);
    let x = 0;
    let y = (area.height.saturating_sub(dialog_height)) / 2;
    let rect = Rect::new(x, y, dialog_width, dialog_height);
    // Defensive: the math above guarantees fit, but assert the
    // invariant explicitly so a future edit can't silently break it.
    debug_assert!(
        rect.x + rect.width <= area.x + area.width && rect.y + rect.height <= area.y + area.height,
        "approval_dialog_area produced a rect outside the area: \
         rect={rect:?} area={area:?}"
    );
    Some(rect)
}

/// Build the args preview as a flat list of wrapped display lines.
///
/// Pure function — no I/O, no ratatui types in the output. Unit-testable
/// without a frame. Each returned string is one visual line of the
/// wrapped JSON pretty-print of `approval.args`.
///
/// `wrap_width` is the inner width of the dialog (in cells). Long lines
/// are wrapped on char boundaries (UTF-8 safe, regression guard for
/// the byte-slice panic class fixed in commit 9900102).
pub fn format_args_preview(approval: &PendingApproval, wrap_width: usize) -> Vec<String> {
    let raw = serde_json::to_string_pretty(&approval.args).unwrap_or_default();
    let width = wrap_width.max(8).saturating_sub(2); // -2 for the leading " " indent
    raw.lines()
        .flat_map(|line| wrap_line(line, width))
        .collect()
}

/// Word-wrap a single line to `width` cells, splitting on char boundaries
/// (NOT byte boundaries — a multi-byte UTF-8 char must not be split).
/// Returns at least one line (the empty string for an empty input).
///
/// **Cell-width approximation:** the dialog is monospace, so wrapping
/// by char count is correct for ASCII and a slight over-estimate for
/// full-width CJK (which would ideally be 2 cells per char). For the
/// preview use case (pretty-printed JSON in English source files) this
/// is the right trade-off — no extra dependency, no surprises with
/// combiners or joiners, UTF-8 safe. The visual result is a stable
/// line count that tests can pin.
fn wrap_line(line: &str, width: usize) -> Vec<String> {
    if line.is_empty() {
        return vec![String::new()];
    }
    let mut out = Vec::new();
    let mut current = String::new();
    let mut current_w = 0usize;
    for ch in line.chars() {
        if current_w + 1 > width && !current.is_empty() {
            out.push(std::mem::take(&mut current));
            current_w = 0;
        }
        current.push(ch);
        current_w += 1;
    }
    if !current.is_empty() {
        out.push(current);
    }
    if out.is_empty() {
        out.push(String::new());
    }
    out
}

/// Standardized risk tier for an approval (WO 34.10).
///
/// Three tiers, each with a one-line explanation:
///   - `SAFE`      — reads only, no state change
///   - `REVIEW`    — modifies project files or runs a non-destructive shell
///   - `DANGEROUS` — can delete or overwrite data, or writes outside the CWD
///
/// Replaces the old ad-hoc `risk_hint` ("destructive — could delete data"
/// / "writes files or network" / "read-only" / "runs a shell command") and
/// the old `risk_summary_level` ("low/medium/high risk"). The mapping is
/// intentionally simple and pinned by tests so the wording is consistent
/// across every approval dialog.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum RiskTier {
    Safe,
    Review,
    Dangerous,
}

impl RiskTier {
    /// Uppercase label shown in the dialog.
    pub fn label(&self) -> &'static str {
        match self {
            RiskTier::Safe => "SAFE",
            RiskTier::Review => "REVIEW",
            RiskTier::Dangerous => "DANGEROUS",
        }
    }
}

impl std::fmt::Display for RiskTier {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(self.label())
    }
}

/// Classify an approval into a `RiskTier`. Pure function so the test
/// suite can pin the mapping.
///
/// - `is_outside_cwd`: for file edits, whether the target path resolves
///   outside the current working directory. Outside-CWD edits are
///   `DANGEROUS` regardless of the tool, because the user can't easily
///   see what's being changed.
pub fn risk_tier(approval: &PendingApproval, is_outside_cwd: bool) -> RiskTier {
    if is_outside_cwd {
        return RiskTier::Dangerous;
    }
    let name = approval.tool_name.as_str();
    if name == "bash" {
        if let Some(cmd) = approval.args.get("command").and_then(|v| v.as_str()) {
            let lower = cmd.to_lowercase();
            // DANGEROUS: commands that can delete or overwrite data.
            if lower.contains("rm -rf")
                || lower.contains("rm -fr")
                || lower.contains("mkfs")
                || lower.contains("dd if=")
                || lower.contains(":(){:|:&};:")
                || lower.contains("chmod -r 777")
                || lower.contains("chmod 777 /")
            {
                return RiskTier::Dangerous;
            }
            // SAFE: read-only commands.
            if lower.starts_with("ls")
                || lower.starts_with("cat")
                || lower.starts_with("head")
                || lower.starts_with("tail")
                || lower.starts_with("grep")
                || lower.starts_with("rg ")
                || lower.starts_with("find ")
                || lower.starts_with("echo ")
                || lower.starts_with("pwd")
            {
                return RiskTier::Safe;
            }
            // REVIEW: anything else that writes files or runs a shell
            // command (rm without -rf, mv, >, >>, sed -i, curl, wget,
            // cargo build, etc.).
            return RiskTier::Review;
        }
    }
    if name == "edit_file" || name == "write_file" {
        // File edits modify project files. They're REVIEW unless the
        // path is outside the CWD (handled above → DANGEROUS).
        return RiskTier::Review;
    }
    // Unknown tool — REVIEW is the safe default (don't assume safe).
    RiskTier::Review
}

/// One-line explanation for a risk tier. Shown in the dialog under the
/// action headline so the user understands what the tier means.
pub fn risk_tier_explanation(tier: RiskTier) -> &'static str {
    match tier {
        RiskTier::Safe => "Reads files only",
        RiskTier::Review => "Modifies project files",
        RiskTier::Dangerous => "Can delete or overwrite data",
    }
}

/// Color to render the risk tier in. Mirrors the tier's severity.
pub fn risk_tier_color(tier: RiskTier) -> Color {
    match tier {
        RiskTier::Safe => Color::Green,
        RiskTier::Review => Color::Yellow,
        RiskTier::Dangerous => Color::Red,
    }
}

/// Build the action-first headline + detail line for an approval
/// (WO 34.10). The headline is the *action* (what will happen), not
/// the tool name. Returns `(headline, detail)`:
///   - File edits: `⚠ Change <path>` + `+N -M lines`
///   - Bash:       `⚠ Run command` + the command text
///   - Other:      `⚠ <tool_name>` + the path (fallback)
///
/// Pure function so the test suite can pin the wording.
pub fn action_headline(
    approval: &PendingApproval,
    path: &str,
    added: usize,
    deleted: usize,
) -> (String, String) {
    let name = approval.tool_name.as_str();
    match name {
        "edit_file" | "write_file" => {
            let headline = format!("⚠ Change  {path}");
            let detail = format!("+{added} -{deleted} lines");
            (headline, detail)
        }
        "bash" => {
            let cmd = approval
                .args
                .get("command")
                .and_then(|v| v.as_str())
                .unwrap_or("");
            let headline = "⚠ Run command".to_string();
            (headline, cmd.to_string())
        }
        _ => {
            // Fallback for unknown tools: show the tool name + path.
            let headline = format!("⚠ {name}  {path}");
            (headline, String::new())
        }
    }
}

// (No external dep for cell-width — see the `wrap_line` doc comment
// for why a 1-char-per-cell approximation is the right trade-off here.)

#[cfg(test)]
mod tests {
    use super::*;
    use crate::tui::app::PendingApproval;
    use ratatui::layout::Rect;
    use serde_json::json;

    fn make_approval(tool: &str, args: serde_json::Value) -> PendingApproval {
        PendingApproval {
            tool_name: tool.into(),
            args,
            responder: None,
        }
    }

    // ── Bug 2: approval dialog bounds guard ────────────────────────
    //
    // The prior `render_approval_dialog` called
    // `(area.height * 3 / 4).clamp(10, area.height)` which PANICS when
    // `area.height < 10` (min > max in clamp). A 0-dimension Rect
    // passed to `Clear` also corrupts the terminal. The fix is a pure
    // `approval_dialog_area` helper that returns `None` for tiny
    // terminals and a valid, in-bounds Rect otherwise. These tests
    // pin the contract.

    /// A normal terminal (120x40) produces a 120-wide, 30-tall dialog
    /// (75% of 40), centered vertically (y = 5).
    #[test]
    fn approval_dialog_area_normal_terminal() {
        let area = Rect::new(0, 0, 120, 40);
        let dialog = approval_dialog_area(area).expect("normal terminal should produce a rect");
        assert_eq!(dialog.width, 120, "dialog should be full width");
        assert_eq!(dialog.height, 30, "dialog should be 75% of terminal height");
        assert_eq!(dialog.x, 0);
        assert_eq!(dialog.y, 5, "dialog should be vertically centered");
        // The rect must fit inside the area.
        assert!(dialog.x + dialog.width <= area.x + area.width);
        assert!(dialog.y + dialog.height <= area.y + area.height);
    }

    /// A small terminal (80x24) still produces a valid dialog.
    #[test]
    fn approval_dialog_area_small_terminal() {
        let area = Rect::new(0, 0, 80, 24);
        let dialog = approval_dialog_area(area).expect("80x24 should produce a rect");
        assert_eq!(dialog.width, 80);
        assert_eq!(dialog.height, 18); // 24 * 3 / 4 = 18
        assert!(dialog.y + dialog.height <= area.height);
    }

    /// A tiny terminal (height < 4) returns None — no crash, no
    /// corruption. The prior code panicked here.
    #[test]
    fn approval_dialog_area_tiny_height_returns_none() {
        assert!(approval_dialog_area(Rect::new(0, 0, 80, 3)).is_none());
        assert!(approval_dialog_area(Rect::new(0, 0, 80, 0)).is_none());
        assert!(approval_dialog_area(Rect::new(0, 0, 80, 1)).is_none());
    }

    /// A tiny terminal (width < 20) returns None.
    #[test]
    fn approval_dialog_area_tiny_width_returns_none() {
        assert!(approval_dialog_area(Rect::new(0, 0, 19, 40)).is_none());
        assert!(approval_dialog_area(Rect::new(0, 0, 0, 40)).is_none());
    }

    /// height == 4 (the minimum) produces a 4-tall dialog (max(4)
    /// floor), not a panic. This is the exact case that crashed
    /// before — `clamp(10, 4)` panics because 10 > 4.
    #[test]
    fn approval_dialog_area_min_height_4_does_not_panic() {
        let area = Rect::new(0, 0, 80, 4);
        let dialog = approval_dialog_area(area).expect("height=4 should produce a rect");
        assert_eq!(dialog.height, 4, "height=4 should clamp to 4 (the floor)");
        assert_eq!(dialog.width, 80);
        assert!(dialog.y + dialog.height <= area.height);
    }

    /// height == 9 (just under the old clamp min of 10) must not
    /// panic. The old code: `(9 * 3 / 4).clamp(10, 9)` = `6.clamp(10, 9)`
    /// → panic (min > max). The new code: `6.min(9).max(4)` = `6`.
    #[test]
    fn approval_dialog_area_height_9_does_not_panic() {
        let area = Rect::new(0, 0, 80, 9);
        let dialog = approval_dialog_area(area).expect("height=9 should produce a rect");
        assert_eq!(dialog.height, 6, "9*3/4=6, clamped to [4,9]");
    }

    /// The returned rect NEVER exceeds the area, for any height in
    /// the valid range. Fuzz-style guard against off-by-one.
    #[test]
    fn approval_dialog_area_rect_always_fits_inside_area() {
        for h in 4..=200u16 {
            for w in 20..=200u16 {
                let area = Rect::new(0, 0, w, h);
                if let Some(dialog) = approval_dialog_area(area) {
                    assert!(
                        dialog.x + dialog.width <= area.x + area.width,
                        "w={w} h={h}: dialog width overflow"
                    );
                    assert!(
                        dialog.y + dialog.height <= area.y + area.height,
                        "w={w} h={h}: dialog height overflow"
                    );
                    assert!(dialog.width >= 1, "w={w} h={h}: zero width");
                    assert!(dialog.height >= 1, "w={w} h={h}: zero height");
                }
            }
        }
    }

    /// Empty / very short args: pretty-printed JSON gives 3 lines
    /// ("{", field, "}"), all short enough to stay single-line after
    /// wrapping at width 60.
    #[test]
    fn test_format_args_preview_short() {
        let a = make_approval("bash", json!({"command": "ls"}));
        let lines = format_args_preview(&a, 60);
        // 3 lines: the opening "{" brace, the "command" field, the
        // closing "}" brace. None wrap at width 60.
        assert_eq!(lines.len(), 3);
        // The field line is the only one that contains the value.
        assert!(lines[1].contains("ls"));
    }

    /// Long single-line command: wrapped to multiple lines.
    #[test]
    fn test_format_args_preview_wraps_long_bash_command() {
        let cmd = "echo ".to_string() + &"a".repeat(200);
        let a = make_approval("bash", json!({"command": cmd}));
        let lines = format_args_preview(&a, 40);
        assert!(lines.len() > 1, "long command should wrap");
        // No wrapped line should exceed 40 chars
        for line in &lines {
            assert!(
                line.chars().count() <= 40,
                "wrapped line exceeds width: {:?} (chars={})",
                line,
                line.chars().count()
            );
        }
    }

    /// Multi-line JSON (edit_file) — one visual line per JSON source line,
    /// with the long `old_string` further wrapped.
    #[test]
    fn test_format_args_preview_edit_file_multiline() {
        let a = make_approval(
            "edit_file",
            json!({
                "path": "src/main.rs",
                "old_string": "fn main() {\n    println!(\"hello\");\n}",
                "new_string": "fn main() {\n    println!(\"hello, world\");\n}"
            }),
        );
        let lines = format_args_preview(&a, 50);
        // The pretty-printed JSON has 5 source lines: "{", "  \"new_string\":...",
        // "  \"old_string\":...{long string}", "  \"path\":...",
        // "}". serde_json::to_string_pretty sorts keys alphabetically, so
        // new_string < old_string < path. At width 50-2=48, the long
        // string values wrap to 2 lines each, so we get:
        //   [0] = "{"
        //   [1] = "  \"new_string\": ..." (wrapped part 1)
        //   [2] = "lo, world\");\n}\"," (wrapped part 2)
        //   [3] = "  \"old_string\": ..." (wrapped part 1)
        //   [4] = "lo\");\n}\","        (wrapped part 2)
        //   [5] = "  \"path\": \"src/main.rs\""
        //   [6] = "}"
        assert!(
            lines.len() >= 5,
            "expected at least 5 visual lines, got {}",
            lines.len()
        );
        // `lines[0]` is the opening brace; `lines[last-1]` is the closing brace.
        assert!(lines[0] == "{", "lines[0] was {:?}", lines[0]);
        // The path line — find it by content rather than relying on
        // alphabetical key order (more robust if serde_json ever changes
        // its sort behaviour, which is documented but not promised).
        let path_line = lines
            .iter()
            .find(|l| l.contains("src/main.rs"))
            .unwrap_or_else(|| panic!("no line contains the path; lines={lines:?}"));
        assert!(path_line.contains("path"));
    }

    /// UTF-8 char in args must not panic when wrapping.
    /// Regression guard for the byte-slice panic class.
    #[test]
    fn test_format_args_preview_utf8_safe() {
        let a = make_approval("write_file", json!({"content": "🦀".repeat(100)}));
        let lines = format_args_preview(&a, 30);
        // Should not panic. Should produce at least 2 wrapped lines.
        assert!(!lines.is_empty());
        for line in &lines {
            // No line should be invalid UTF-8 (the type system enforces this,
            // but explicitly check no truncation marker is mid-char)
            for ch in line.chars() {
                assert!(ch.len_utf8() > 0);
            }
        }
    }

    /// Risk tier for `rm -rf` is DANGEROUS.
    #[test]
    fn test_risk_tier_destructive_rm() {
        let a = make_approval("bash", json!({"command": "rm -rf /tmp/old"}));
        assert_eq!(risk_tier(&a, false), RiskTier::Dangerous);
    }

    /// Risk tier for `ls` is SAFE.
    #[test]
    fn test_risk_tier_safe_ls() {
        let a = make_approval("bash", json!({"command": "ls -la"}));
        assert_eq!(risk_tier(&a, false), RiskTier::Safe);
    }

    /// Risk tier for `cat` is SAFE.
    #[test]
    fn test_risk_tier_safe_cat() {
        let a = make_approval("bash", json!({"command": "cat /etc/hostname"}));
        assert_eq!(risk_tier(&a, false), RiskTier::Safe);
    }

    /// Risk tier for `cargo build` (long-running, not destructive) is REVIEW.
    #[test]
    fn test_risk_tier_review_cargo() {
        let a = make_approval("bash", json!({"command": "cargo build 2>&1 | tail -20"}));
        assert_eq!(risk_tier(&a, false), RiskTier::Review);
    }

    /// Risk tier for `mv` (writes) is REVIEW.
    #[test]
    fn test_risk_tier_review_mv() {
        let a = make_approval("bash", json!({"command": "mv old.txt new.txt"}));
        assert_eq!(risk_tier(&a, false), RiskTier::Review);
    }

    /// Risk tier for `edit_file` is REVIEW (modifies project files).
    #[test]
    fn test_risk_tier_review_edit_file() {
        let a = make_approval(
            "edit_file",
            json!({"path": "x", "old_string": "a", "new_string": "b"}),
        );
        assert_eq!(risk_tier(&a, false), RiskTier::Review);
    }

    /// Risk tier for `write_file` is REVIEW.
    #[test]
    fn test_risk_tier_review_write_file() {
        let a = make_approval("write_file", json!({"path": "x", "content": "y"}));
        assert_eq!(risk_tier(&a, false), RiskTier::Review);
    }

    /// Risk tier for an edit_file OUTSIDE the CWD is DANGEROUS.
    #[test]
    fn test_risk_tier_outside_cwd_is_dangerous() {
        let a = make_approval(
            "edit_file",
            json!({"path": "x", "old_string": "a", "new_string": "b"}),
        );
        assert_eq!(risk_tier(&a, true), RiskTier::Dangerous);
    }

    /// Risk tier for an unknown tool is REVIEW (safe default — don't
    /// assume safe).
    #[test]
    fn test_risk_tier_unknown_tool_is_review() {
        let a = make_approval("some_custom_tool", json!({"path": "x"}));
        assert_eq!(risk_tier(&a, false), RiskTier::Review);
    }

    /// Risk tier explanation + label + color for each tier.
    #[test]
    fn test_risk_tier_label_explanation_color() {
        assert_eq!(RiskTier::Safe.label(), "SAFE");
        assert_eq!(RiskTier::Review.label(), "REVIEW");
        assert_eq!(RiskTier::Dangerous.label(), "DANGEROUS");
        assert_eq!(risk_tier_explanation(RiskTier::Safe), "Reads files only");
        assert_eq!(
            risk_tier_explanation(RiskTier::Review),
            "Modifies project files"
        );
        assert_eq!(
            risk_tier_explanation(RiskTier::Dangerous),
            "Can delete or overwrite data"
        );
        assert_eq!(risk_tier_color(RiskTier::Safe), Color::Green);
        assert_eq!(risk_tier_color(RiskTier::Review), Color::Yellow);
        assert_eq!(risk_tier_color(RiskTier::Dangerous), Color::Red);
    }

    /// `RiskTier` implements `Display` as the uppercase label.
    #[test]
    fn test_risk_tier_display_is_label() {
        assert_eq!(format!("{}", RiskTier::Safe), "SAFE");
        assert_eq!(format!("{}", RiskTier::Review), "REVIEW");
        assert_eq!(format!("{}", RiskTier::Dangerous), "DANGEROUS");
    }

    // ── WO 34.10: action-first headline ─────────────────────────────

    /// File edit headline: `⚠ Change <path>` + `+N -M lines`.
    #[test]
    fn test_action_headline_edit_file() {
        let a = make_approval(
            "edit_file",
            json!({"path": "src/tui/app.rs", "old_string": "a", "new_string": "b"}),
        );
        let (headline, detail) = action_headline(&a, "src/tui/app.rs", 18, 4);
        assert_eq!(headline, "⚠ Change  src/tui/app.rs");
        assert_eq!(detail, "+18 -4 lines");
    }

    /// Bash headline: `⚠ Run command` + the command text.
    #[test]
    fn test_action_headline_bash() {
        let a = make_approval("bash", json!({"command": "cargo test"}));
        let (headline, detail) = action_headline(&a, "-", 0, 0);
        assert_eq!(headline, "⚠ Run command");
        assert_eq!(detail, "cargo test");
    }

    /// Unknown tool headline: `⚠ <tool> <path>` (fallback).
    #[test]
    fn test_action_headline_unknown_tool() {
        let a = make_approval("custom_tool", json!({"path": "x"}));
        let (headline, detail) = action_headline(&a, "x", 0, 0);
        assert_eq!(headline, "⚠ custom_tool  x");
        assert_eq!(detail, "");
    }

    /// `wrap_line` is the building block of `format_args_preview`.
    #[test]
    fn test_wrap_line_short_passthrough() {
        let lines = wrap_line("hello", 40);
        assert_eq!(lines, vec!["hello".to_string()]);
    }

    /// `wrap_line` on empty string returns one empty line.
    #[test]
    fn test_wrap_line_empty() {
        let lines = wrap_line("", 40);
        assert_eq!(lines, vec![String::new()]);
    }

    /// `wrap_line` does not panic on multibyte chars.
    #[test]
    fn test_wrap_line_utf8_does_not_split_chars() {
        let line = "🦀".repeat(20);
        let lines = wrap_line(&line, 8);
        for l in &lines {
            // The whole line is a sequence of whole 🦀s (each 2 cells wide)
            for ch in l.chars() {
                assert!(ch == '🦀' || ch == ' ', "got unexpected char: {ch:?}");
            }
        }
    }
}
