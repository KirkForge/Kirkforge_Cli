//! Build the FFmpeg `-filter_complex` string for a sequence of scenes.
//!
//! Strategy: render each scene as its own self-contained chain ending in a
//! labeled output `[v_n]`, then concatenate them with `concat=n=N:v=1:a=0`.
//! This is the only way to chain multiple drawtext-heavy scenes — emitting
//! them as parallel chains with one `[vout]` from the last scene leaves
//! intermediate outputs "unconnected" and ffmpeg rejects the graph.

use crate::compose::{BrandTheme, Scene, TerminalStep};

#[derive(Debug)]
pub struct FilterPlan {
    pub filter_complex: String,
    pub inputs: Vec<String>,
}

/// ponytail: tiny hex → (r,g,b) helper used by the pie chart to build
/// `geq` color expressions. Accepts `#RRGGBB` (with or without the `#`).
/// Returns (0,0,0) on any parse error so a malformed color doesn't blow
/// up the whole render — the slice will just appear black, which the
/// user will spot immediately on preview.
fn hex_to_rgb(s: &str) -> (u8, u8, u8) {
    let h = s.trim_start_matches('#');
    if h.len() != 6 {
        return (0, 0, 0);
    }
    let Ok(r) = u8::from_str_radix(&h[0..2], 16) else {
        return (0, 0, 0);
    };
    let Ok(g) = u8::from_str_radix(&h[2..4], 16) else {
        return (0, 0, 0);
    };
    let Ok(b) = u8::from_str_radix(&h[4..6], 16) else {
        return (0, 0, 0);
    };
    (r, g, b)
}

/// Escape text for FFmpeg `drawtext=` (backslash, colon, percent, single-quote).
///
/// ponytail: we pass the filter as a single argv token via tokio's
/// `Command` API, not through a shell, so we emit ffmpeg-native escapes
/// (`\:` for `:`, `\\` for `\`, `\%` for `%`, `\'` for `'`) — not the
/// 6-char `'\''` shell-style sequence that older code emitted (which
/// ffmpeg's parser does not understand and rejects with "No such filter: ''").
fn escape_drawtext(s: &str) -> String {
    s.replace('\\', "\\\\")
        .replace(':', "\\:")
        .replace('%', "\\%")
        // ponytail: a literal apostrophe inside `text='...'`. ffmpeg's
        // filter parser sees `'` as ending the string, and `\'` is NOT
        // a level-0 escape (only `\:` `\%` `\,` `\\` are). The level-1
        // escape `\x27` (hex) survives a round-trip and is the only
        // form that renders an actual apostrophe. Tested standalone.
        .replace('\'', "\\x27")
        .replace(',', "\\,")
}

/// ponytail: drawtext options are split on whitespace, so any value
/// passed to `x=`, `y=`, etc. that contains an expression like
/// `max(40, (w - text_w)/2)` must have its spaces (and the `,` between
/// args) escaped — `\,\ ` and `\\ ` — or ffmpeg parses the next token
/// as the next option/filter. This wraps a raw expression string.
fn escape_drawtext_expr(s: &str) -> String {
    s.replace('\\', "\\\\")
        .replace(',', "\\,")
        .replace(' ', "\\ ")
}

fn shell_quote(s: String) -> String {
    if s.contains(' ') || s.contains('\'') {
        format!("'{}'", s.replace('\'', "'\\''"))
    } else {
        s
    }
}

/// ponytail: camera_motion → optional filter chunk applied to the scene's
/// `color=...` background source before drawtext. Ken Burns is implemented
/// by scaling the bg to 1.15× and cropping with a `t`-based expression;
/// that gives a smooth zoom on the bg while drawtext stays anchored. Empty
/// string = static, no extra filter.
fn motion_filter(camera_motion: Option<&str>, width: u32, height: u32, dur: f32) -> String {
    // Scale factor 1.15 = 15% headroom for crop-pan.
    let sw = (width as f32 * 1.15) as u32;
    let sh = (height as f32 * 1.15) as u32;
    let t = format!("{dur}");
    match camera_motion {
        Some("push") => {
            format!("scale={sw}:{sh},crop={width}:{height}:x='(iw-{width})/2':y='(ih-{height})/2',",)
        }
        Some("pan") => format!(
            "scale={sw}:{sh},crop={width}:{height}:x='(iw-{width})*t/{t}':y='(ih-{height})/2',",
        ),
        Some("fade") => format!(
            "fade=t=in:st=0:d=0.5,fade=t=out:st={so}:d=0.5,",
            so = (dur - 0.5).max(0.0)
        ),
        _ => String::new(),
    }
}

pub fn build_filter_graph(scenes: &[Scene], width: u32, height: u32, fps: u32) -> FilterPlan {
    build_filter_graph_with_brand(scenes, width, height, fps, &BrandTheme::default())
}

/// ponytail: variants that override the default brand color (from
/// `<project>/brand.json`). 99% of callers can use the 4-arg wrapper
/// above; render.rs is the one place that actually threads the project
/// brand through.
pub fn build_filter_graph_with_brand(
    scenes: &[Scene],
    width: u32,
    height: u32,
    fps: u32,
    brand: &BrandTheme,
) -> FilterPlan {
    let mut chains = Vec::new();
    let mut inputs: Vec<String> = Vec::new();
    let mut input_idx = 0u32;

    for (i, scene) in scenes.iter().enumerate() {
        let out = format!("v{i}");
        let dur = scene_duration(scene);
        let label = format!("v{i}_label");
        let mut chain = String::new();
        match scene {
            Scene::ClipCut {
                src, in_s, out_s, ..
            } => {
                inputs.push(format!(
                    "-ss {in_s} -t {} -i {}",
                    out_s - in_s,
                    shell_quote(src.display().to_string())
                ));
                let idx = input_idx;
                // ponytail: scale + letterbox to match the composition. Without
                // this, concat refuses mismatched sizes and the whole render
                // blows up. The clip is treated as letterboxed content; could
                // later be a `mode: stretch|crop` field.
                chain.push_str(&format!(
                    "[{idx}:v]setpts=PTS-STARTPTS,format=yuv420p,\
                     scale={width}:{height}:force_original_aspect_ratio=decrease,\
                     pad={width}:{height}:(ow-iw)/2:(oh-ih)/2:black[{out}]"
                ));
                input_idx += 1;
            }
            Scene::HeroTitle {
                text,
                subtitle,
                shot,
                ..
            } => {
                chain.push_str(&format!(
                    "color=black:s={width}x{height}:r={fps}:d={dur}[bg{i}];"
                ));
                let motion = motion_filter(
                    shot.as_ref().and_then(|m| m.camera_motion.as_deref()),
                    width,
                    height,
                    dur,
                );
                chain.push_str(&format!("[bg{i}]{motion}drawtext=text="));
                let escaped = escape_drawtext(text);
                let voff = if subtitle.is_some() { height / 8 } else { 0 };
                chain.push_str(&format!(
                    "'{escaped}':fontcolor=white:fontsize={fsz}:\
                     x={x_expr}:y={y_expr}[{label}];",
                    x_expr = escape_drawtext_expr("(w-text_w)/2"),
                    y_expr = escape_drawtext_expr(&format!("(h-text_h)/2 - {voff}")),
                    fsz = (height / 8).max(48),
                ));
                if let Some(sub) = subtitle {
                    let sub_esc = escape_drawtext(sub);
                    chain.push_str(&format!(
                        "[{label}]drawtext=text='{sub_esc}':fontcolor=#cccccc:fontsize={fsz}:\
                         x={x_expr}:y={y_expr}[{out}]",
                        x_expr = escape_drawtext_expr("(w-text_w)/2"),
                        y_expr = escape_drawtext_expr(&format!("(h-text_h)/2 + {voff}")),
                        fsz = (height / 18).max(20),
                    ));
                } else {
                    chain.push_str(&format!("[{label}]copy[{out}]"));
                }
            }
            Scene::TextCard {
                title, body, shot, ..
            } => {
                chain.push_str(&format!(
                    "color=black:s={width}x{height}:r={fps}:d={dur}[bg{i}];"
                ));
                let motion = motion_filter(
                    shot.as_ref().and_then(|m| m.camera_motion.as_deref()),
                    width,
                    height,
                    dur,
                );
                let t = escape_drawtext(title);
                let b = escape_drawtext(body);
                chain.push_str(&format!(
                    "[bg{i}]{motion}drawtext=text='{t}':fontcolor=white:fontsize={fsz}:x=80:y=80[{label}];",
                    fsz = (height / 12).max(40),
                ));
                chain.push_str(&format!(
                    "[{label}]drawtext=text='{b}':fontcolor=#dddddd:fontsize={fsz}:x=80:y={ty}[{out}]",
                    fsz = (height / 22).max(18),
                    ty = height / 5,
                ));
            }
            Scene::TerminalScene {
                title,
                prompt,
                accent_color,
                steps,
                shot,
                ..
            } => {
                // Synthetic terminal: window chrome + prompt + typed commands
                // + instantaneous output lines. Time-based reveal is approximated
                // by stamping each line at its cumulative cursor offset; on a
                // single output frame this gives a static "fully typed" look
                // (the animation is driven by scene duration in the timeline).
                chain.push_str(&format!(
                    "color=black:s={width}x{height}:r={fps}:d={dur}[bg{i}];"
                ));
                let motion = motion_filter(
                    shot.as_ref().and_then(|m| m.camera_motion.as_deref()),
                    width,
                    height,
                    dur,
                );
                // Window chrome: mac-style dark rounded-ish box centered.
                let pad = (height / 8) as i32;
                let win_x = pad;
                let win_y = pad;
                let win_w = (width as i32) - 2 * pad;
                let win_h = (height as i32) - 2 * pad;
                chain.push_str(&format!(
                    "[bg{i}]{motion}drawbox=x={wx}:y={wy}:w={ww}:h={wh}:\
                     color={chrome}@0.95:t=fill[win0];",
                    wx = win_x,
                    wy = win_y,
                    ww = win_w,
                    wh = win_h,
                    chrome = "#1e1e22",
                ));
                // Title bar background.
                chain.push_str(&format!(
                    "[win0]drawbox=x={wx}:y={wy}:w={ww}:h=44:\
                     color={bar}@1.0:t=fill[win1];",
                    wx = win_x,
                    wy = win_y,
                    ww = win_w,
                    bar = "#2d2d33",
                ));
                // Traffic-light dots. ponytail: each dot chains off
                // the previous (`win{n}` → `win{n+1}`), so the chain is
                // a linear pipeline. Parallel branches on `[win1]`
                // confuse ffmpeg's filter parser when the same label is
                // consumed more than once.
                let mut dot_prev = 1u32;
                for (n, c) in ["#ff5f56", "#ffbd2e", "#27c93f"].iter().enumerate() {
                    let dot_next = n as u32 + 2;
                    chain.push_str(&format!(
                        "[win{dot_prev}]drawbox=x={dx}:y={dy}:w=14:h=14:color={col}@1.0:t=fill[win{dot_next}];",
                        dx = win_x + 18 + (n as i32) * 24,
                        dy = win_y + 15,
                        col = c,
                    ));
                    dot_prev = dot_next;
                }
                let next_label = 5;
                // Title text (centered on title bar).
                if let Some(t) = title {
                    chain.push_str(&format!(
                        "[win4]drawtext=text='{t}':fontcolor=#cccccc:fontsize=20:\
                         x=(w-text_w)/2:y={ty}[win{next_label}];",
                        ty = win_y + 11,
                    ));
                } else {
                    chain.push_str(&format!("[win4]null[win{next_label}];"));
                }
                // Accent color for prompt: brand.primary_color if not set.
                let accent = accent_color
                    .as_deref()
                    .unwrap_or(brand.primary_color.as_str());
                // Build lines: each is either a typed command (`prompt text`)
                // or an output line. Pills render as a top-right badge.
                let mut lines: Vec<(String, bool)> = Vec::new(); // (text, is_cmd)
                let mut pills: Vec<(String, String)> = Vec::new(); // (text, color)
                for step in steps {
                    match step {
                        TerminalStep::Cmd { text, .. } => {
                            lines.push((format!("{prompt} {text}"), true));
                        }
                        TerminalStep::Out { text, .. } => {
                            lines.push((text.clone(), false));
                        }
                        TerminalStep::Pause { .. } => {}
                        TerminalStep::Pill { text, color, .. } => {
                            let c = color
                                .as_deref()
                                .unwrap_or(brand.primary_color.as_str())
                                .to_string();
                            pills.push((text.clone(), c));
                        }
                    }
                }
                // Lay out terminal body lines starting below the title bar.
                let body_x = win_x + 24;
                let body_y0 = win_y + 64;
                let line_h = 28;
                let max_lines = ((win_h - 80) / line_h).max(1) as usize;
                let truncated = lines.len() > max_lines;
                let visible = if truncated {
                    &lines[..max_lines.saturating_sub(1)]
                } else {
                    &lines[..]
                };
                if truncated {
                    let last_idx = visible.len();
                    chain.push_str(&format!(
                        "[win{prev}]drawtext=text='... ({} more lines)':\
                         fontcolor=#777777:fontsize=18:x={bx}:y={by}[win{prev}];",
                        lines.len() - visible.len(),
                        bx = body_x,
                        by = body_y0 + (last_idx as i32) * line_h,
                        prev = next_label,
                    ));
                }
                let mut prev = format!("win{next_label}");
                for (li, (text, is_cmd)) in visible.iter().enumerate() {
                    let esc = escape_drawtext(text);
                    let color = if *is_cmd { accent } else { "#d4d4d4" };
                    let new_label = if li + 1 == visible.len() {
                        out.clone()
                    } else {
                        format!("tw{li}")
                    };
                    chain.push_str(&format!(
                        "[{prev}]drawtext=text='{esc}':fontcolor={col}:fontsize=20:\
                         x={bx}:y={by}[{new_label}];",
                        col = color,
                        bx = body_x,
                        by = body_y0 + (li as i32) * line_h,
                    ));
                    prev = new_label;
                }
                // If the last line label wasn't `out`, chain a null pad.
                if prev != out {
                    chain.push_str(&format!("[{prev}]null[{out}];"));
                }
                // Pills stack top-right, smallest duration drawn first.
                if !pills.is_empty() {
                    // Render the first pill at the rightmost position; later
                    // pills (overrides) cover it via drawbox. For a single
                    // shot frame we only need the final pill visible.
                    let (pt, pc) = pills
                        .last()
                        .map(|(t, c)| (t.clone(), c.clone()))
                        .unwrap_or_else(|| ("".into(), brand.primary_color.clone()));
                    if !pt.is_empty() {
                        let esc = escape_drawtext(&pt);
                        let pill_w = (pt.chars().count() as i32 * 14).max(80) + 32;
                        let pill_h = 40;
                        let pill_x = (width as i32) - win_x - pill_w - 16;
                        let pill_y = win_y - pill_h / 2 + 4;
                        chain.push_str(&format!(
                            "[{out}]drawbox=x={pill_x}:y={pill_y}:w={pill_w}:h={pill_h}:\
                             color={pc}@1.0:t=fill[pill_bg];",
                        ));
                        chain.push_str(&format!(
                            "[pill_bg]drawtext=text='{esc}':fontcolor=white:fontsize=20:\
                             x=(w-text_w)/2:y={ty}[{out}];",
                            ty = pill_y + 9,
                        ));
                    }
                }
            }
            Scene::StatCard {
                number,
                label: lab,
                shot,
                ..
            } => {
                chain.push_str(&format!(
                    "color=black:s={width}x{height}:r={fps}:d={dur}[bg{i}];"
                ));
                let motion = motion_filter(
                    shot.as_ref().and_then(|m| m.camera_motion.as_deref()),
                    width,
                    height,
                    dur,
                );
                let n = escape_drawtext(number);
                let l = escape_drawtext(lab);
                chain.push_str(&format!(
                    "[bg{i}]{motion}drawtext=text='{n}':fontcolor={primary}:fontsize={fsz}:\
                     x={x_expr}:y={y_expr}[{label}];",
                    x_expr = escape_drawtext_expr("(w-text_w)/2"),
                    y_expr = escape_drawtext_expr("(h-text_h)/2 - 60"),
                    fsz = (height / 5).max(120),
                    primary = brand.primary_color.as_str(),
                ));
                chain.push_str(&format!(
                    "[{label}]drawtext=text='{l}':fontcolor=white:fontsize={fsz}:\
                     x={x_expr}:y={y_expr}[{out}]",
                    x_expr = escape_drawtext_expr("(w-text_w)/2"),
                    y_expr = escape_drawtext_expr("(h-text_h)/2 + 80"),
                    fsz = (height / 14).max(32),
                ));
            }
            Scene::BarChart {
                title, bars, shot, ..
            } => {
                chain.push_str(&format!(
                    "color=black:s={width}x{height}:r={fps}:d={dur}[bg{i}];"
                ));
                let motion = motion_filter(
                    shot.as_ref().and_then(|m| m.camera_motion.as_deref()),
                    width,
                    height,
                    dur,
                );
                let t = escape_drawtext(title);
                let bar_count = bars.len().max(1) as u32;
                let bar_w = ((width - 160) / bar_count).max(20);
                let chart_top = height / 3;
                let chart_h = (height * 2 / 3) - 60;
                chain.push_str(&format!(
                    "[bg{i}]{motion}drawtext=text='{t}':fontcolor=white:fontsize={fsz}:x=80:y=80[{label}];",
                    fsz = (height / 16).max(36),
                ));
                let mut cur = label.clone();
                for (bi, bar) in bars.iter().enumerate() {
                    let bh = ((bar.value.clamp(0.0, 1.0)) * chart_h as f32) as u32;
                    let x = 80 + (bi as u32) * bar_w + 10;
                    let y = chart_top + (chart_h - bh);
                    let next = format!("b{i}_{bi}");
                    chain.push_str(&format!(
                        "[{cur}]drawbox=x={x}:y={y}:w={bw}:h={bh}:color={c}:t=fill[{next}];",
                        bw = bar_w.saturating_sub(20),
                        c = bar.color,
                    ));
                    let lab_esc = escape_drawtext(&bar.label);
                    chain.push_str(&format!(
                        "[{next}]drawtext=text='{lab_esc}':fontcolor=white:\
                         fontsize={fsz}:x={x}:y={ty}[{next}l];",
                        fsz = (height / 26).max(16),
                        ty = chart_top + chart_h + 10,
                    ));
                    cur = format!("{next}l");
                }
                chain.push_str(&format!("[{cur}]copy[{out}]"));
            }
            Scene::CaptionOverlay { lines, shot, .. } => {
                chain.push_str(&format!(
                    "color=black:s={width}x{height}:r={fps}:d={dur}[bg{i}];"
                ));
                let motion = motion_filter(
                    shot.as_ref().and_then(|m| m.camera_motion.as_deref()),
                    width,
                    height,
                    dur,
                );
                let joined = escape_drawtext(&lines.join("  •  "));
                chain.push_str(&format!(
                    "[bg{i}]{motion}drawtext=text='{joined}':fontcolor=white:fontsize={fsz}:\
                     x=(w-text_w)/2:y=h-{yo}[{out}]",
                    fsz = (height / 16).max(36),
                    yo = height / 6,
                ));
            }
            Scene::QuoteCard {
                quote,
                author,
                source,
                shot,
                ..
            } => {
                // ponytail: black bg + large centered quote in the upper
                // third, attribution below. Two-line attribution: author on
                // its own line, optional source italicized smaller.
                chain.push_str(&format!(
                    "color=black:s={width}x{height}:r={fps}:d={dur}[bg{i}];"
                ));
                let motion = motion_filter(
                    shot.as_ref().and_then(|m| m.camera_motion.as_deref()),
                    width,
                    height,
                    dur,
                );
                let q_esc = escape_drawtext(quote);
                let q_label = format!("q{i}");
                chain.push_str(&format!(
                    "[bg{i}]{motion}drawtext=text='{q_esc}':fontcolor=white:fontsize={fsz}:\
                     x=(w-text_w)/2:y={qy}[{q_label}];",
                    fsz = (height / 9).max(48),
                    qy = height / 4,
                ));
                let mut cur = q_label.clone();
                if let Some(a) = author {
                    let a_esc = escape_drawtext(a);
                    let next = format!("a{i}");
                    chain.push_str(&format!(
                        "[{cur}]drawtext=text='— {a_esc}':fontcolor={primary}:fontsize={fsz}:\
                         x=(w-text_w)/2:y={ay}[{next}];",
                        fsz = (height / 16).max(28),
                        ay = (height * 3) / 5,
                        primary = brand.primary_color.as_str(),
                    ));
                    cur = next;
                }
                if let Some(s) = source {
                    let s_esc = escape_drawtext(s);
                    chain.push_str(&format!(
                        "[{cur}]drawtext=text='{s_esc}':fontcolor=#aaaaaa:fontsize={fsz}:\
                         x=(w-text_w)/2:y={sy}[{out}]",
                        fsz = (height / 22).max(20),
                        sy = (height * 3) / 5 + (height / 14).max(28) + 12,
                    ));
                } else {
                    chain.push_str(&format!("[{cur}]copy[{out}]"));
                }
            }
            Scene::LineChart {
                title,
                x_labels,
                series,
                shot,
                ..
            } => {
                // ponytail: title at top, an x-axis tick-row at the bottom,
                // and per-series polyline drawn with `drawbox` segments
                // between consecutive sample points. The plot area sits
                // in the middle 70% of the frame; values are 0..=1 on the
                // y-axis so two series are visually comparable. Segments
                // are thin (3 px) so adjacent series don't smear into a
                // single blob. Distinct palette colors per series when
                // none is supplied.
                chain.push_str(&format!(
                    "color=black:s={width}x{height}:r={fps}:d={dur}[bg{i}];"
                ));
                let motion = motion_filter(
                    shot.as_ref().and_then(|m| m.camera_motion.as_deref()),
                    width,
                    height,
                    dur,
                );
                let mut cur = format!("bg{i}");
                let mb = format!("lcb{i}");
                if motion.is_empty() {
                    // ponytail: empty motion means no filter pass-through.
                    // Emit a plain copy so the pad label exists for the
                    // next filter in the chain. Skipping this entirely
                    // (as we used to) leaves `[mb]` unconnected and the
                    // downstream `[mb]drawtext=...` becomes an empty
                    // filter, which ffmpeg rejects.
                    chain.push_str(&format!("[{cur}]copy[{mb}];"));
                    cur = mb;
                } else {
                    chain.push_str(&format!("[{cur}]{motion}[{mb}];"));
                    cur = mb;
                }
                // Title
                let t_esc = escape_drawtext(title);
                let title_y = 60;
                let t_next = format!("lct{i}");
                chain.push_str(&format!(
                    "[{cur}]drawtext=text='{t_esc}':fontcolor=white:fontsize={fsz}:\
                     x=(w-text_w)/2:y={title_y}[{t_next}];",
                    fsz = (height / 14).max(32),
                ));
                cur = t_next;
                // Plot area geometry: leave 10% margins horizontally and
                // sit between the title and an x-label row at the bottom.
                let margin_x = width / 10;
                let plot_top = title_y + (height / 8).max(80) + 20;
                let label_row_h = (height / 18).max(28);
                let plot_bottom = height.saturating_sub(label_row_h + 20);
                let plot_left = margin_x;
                let plot_right = width.saturating_sub(margin_x);
                let plot_w = plot_right.saturating_sub(plot_left);
                let plot_h = plot_bottom.saturating_sub(plot_top);
                // Faint baseline + top rule so the chart reads as a chart.
                let rule_w = 2;
                let base_next = format!("lcr{i}");
                chain.push_str(&format!(
                    "[{cur}]drawbox=x={plot_left}:y={plot_bottom}:w={plot_w}:h={rule_w}:color=#333333:t=fill[{base_next}];"
                ));
                cur = base_next;
                let top_next = format!("lct2{i}");
                chain.push_str(&format!(
                    "[{cur}]drawbox=x={plot_left}:y={plot_top}:w={plot_w}:h={rule_w}:color=#333333:t=fill[{top_next}];"
                ));
                cur = top_next;
                // X-axis labels (skip if too many to fit).
                let n_x = x_labels.len();
                let step_x = if n_x <= 1 {
                    0
                } else {
                    plot_w / (n_x as u32 - 1)
                };
                let label_fsz = (height / 28).max(16);
                let label_y = plot_bottom + 8;
                for (idx, lab) in x_labels.iter().enumerate() {
                    let lx = plot_left + (idx as u32) * step_x;
                    let l_esc = escape_drawtext(lab);
                    let l_next = format!("lxl{idx}_{i}");
                    chain.push_str(&format!(
                        "[{cur}]drawtext=text='{l_esc}':fontcolor=#cccccc:fontsize={label_fsz}:\
                         x={lx}:y={label_y}[{l_next}];"
                    ));
                    cur = l_next;
                }
                // Default palette (4 colors) — used when a series has no
                // explicit color.
                let palette = ["#3aa0ff", "#ffcc00", "#6cd07a", "#ff5a5a"];
                // Per-series polyline.
                let seg_w = 3;
                for (sidx, s) in series.iter().enumerate() {
                    let color = s
                        .color
                        .clone()
                        .unwrap_or_else(|| palette[sidx % palette.len()].to_string());
                    let n = s.values.len();
                    if n < 2 || step_x == 0 {
                        continue;
                    }
                    let s_step = plot_w / (n as u32 - 1);
                    // Compute the (x, y) sample positions up front so
                    // each pair becomes a single horizontal-ish drawbox
                    // segment. A 3-px-wide drawbox per sample + a thin
                    // connecting box makes the polyline look continuous
                    // without bezier smoothing.
                    for k in 0..(n - 1) {
                        let v0 = s.values[k].clamp(0.0, 1.0);
                        let v1 = s.values[k + 1].clamp(0.0, 1.0);
                        let x0 = plot_left + (k as u32) * s_step;
                        let x1 = plot_left + ((k + 1) as u32) * s_step;
                        let y0 = plot_bottom - ((plot_h as f32) * v0).round() as u32;
                        let y1 = plot_bottom - ((plot_h as f32) * v1).round() as u32;
                        let seg_left = x0.min(x1);
                        let seg_right = x0.max(x1);
                        let seg_top = y0.min(y1);
                        let seg_h = (y0.max(y1) - seg_top).max(2);
                        let next = format!("ls{sidx}_{k}_{i}");
                        chain.push_str(&format!(
                            "[{cur}]drawbox=x={seg_left}:y={seg_top}:w={}:h={seg_h}:color={color}:t=fill[{next}];",
                            seg_right.saturating_sub(seg_left).max(2),
                        ));
                        cur = next;
                    }
                    // Sample dot at the last point so the line has a tip.
                    let last_v = s.values[n - 1].clamp(0.0, 1.0);
                    let lx = plot_left + ((n - 1) as u32) * s_step;
                    let ly = plot_bottom - ((plot_h as f32) * last_v).round() as u32;
                    let next = format!("lsd{sidx}_{i}");
                    chain.push_str(&format!(
                        "[{cur}]drawbox=x={}:y={}:w={seg_w}:h={seg_w}:color={color}:t=fill[{next}];",
                        lx.saturating_sub(seg_w / 2), ly.saturating_sub(seg_w / 2),
                    ));
                    cur = next;
                }
                chain.push_str(&format!("[{cur}]copy[{out}];"));
            }
            Scene::PieChart {
                title,
                slices,
                shot,
                ..
            } => {
                // ponytail: title at top, a circular pie in the left
                // half, a vertical legend on the right. The pie is
                // drawn with the `geq` filter: for each output pixel
                // (x, y), we compute its polar angle relative to the
                // pie's center, decide which slice the angle falls
                // into based on cumulative percentages, and emit the
                // matching color. The legend is composed of one
                // `drawbox` swatch + `drawtext` label per slice.
                chain.push_str(&format!(
                    "color=black:s={width}x{height}:r={fps}:d={dur}[bg{i}];"
                ));
                let motion = motion_filter(
                    shot.as_ref().and_then(|m| m.camera_motion.as_deref()),
                    width,
                    height,
                    dur,
                );
                let mut cur = format!("bg{i}");
                let mb = format!("pcb{i}");
                if motion.is_empty() {
                    chain.push_str(&format!("[{cur}]copy[{mb}];"));
                    cur = mb.clone();
                } else {
                    chain.push_str(&format!("[{cur}]{motion}[{mb}];"));
                    cur = mb.clone();
                }
                let t_esc = escape_drawtext(title);
                let title_y = 60;
                let t_next = format!("pct{i}");
                chain.push_str(&format!(
                    "[{cur}]drawtext=text='{t_esc}':fontcolor=white:fontsize={fsz}:\
                     x=(w-text_w)/2:y={title_y}[{t_next}];",
                    fsz = (height / 14).max(32),
                ));
                cur = t_next;
                // Pie geometry: circle inscribed in the left half, lower
                // 75% of the frame. Center at (cx, cy), radius r.
                let pie_left_pad = width / 8;
                let pie_top = title_y + (height / 8).max(80) + 30;
                let pie_bottom = height.saturating_sub(40);
                let half_w = width / 2;
                let r = (half_w.min(pie_bottom.saturating_sub(pie_top)) / 2).max(80);
                let cx = pie_left_pad + r;
                let cy = pie_top + r;
                let pie_right = cx + r;
                let pie_bottom_px = cy + r;
                let out_w = pie_right.saturating_sub(pie_left_pad);
                let out_h = pie_bottom_px.saturating_sub(pie_top);
                // Normalize percentages to 100.
                let total: f32 = slices.iter().map(|s| s.percent.max(0.0)).sum();
                let palette = [
                    "#3aa0ff", "#ffcc00", "#6cd07a", "#ff5a5a", "#22d3ee", "#ff8c42",
                ];
                // Build three independent geq expressions (r, g, b) that
                // share the same nested-if skeleton but emit different
                // channel values per slice. Outside the circle → 0,0,0
                // so the bounding box shows the bg through.
                let inside_test = format!("lt(hypot(X-{cx}\\,Y-{cy})\\,{r})");
                let angle_expr = format!("mod(atan2(X-{cx}\\,{cy}-Y)*180/PI+360\\,360)");
                let build_channel = |channel_value: &dyn Fn((u8, u8, u8)) -> String| -> String {
                    let mut e = format!("if({inside_test}\\,");
                    if slices.is_empty() || total <= 0.0 {
                        e.push('0');
                    } else {
                        let mut cumulative = 0.0_f32;
                        for (idx, slice) in slices.iter().enumerate() {
                            let pct = slice.percent.max(0.0) / total * 100.0;
                            let start = cumulative;
                            let end = (cumulative + pct).min(100.0);
                            cumulative = end;
                            let color = slice
                                .color
                                .clone()
                                .unwrap_or_else(|| palette[idx % palette.len()].to_string());
                            let rgb = hex_to_rgb(&color);
                            let ch = channel_value(rgb);
                            e.push_str(&format!(
                                "if(between({angle_expr}\\,{start}\\,{end})\\,{ch}\\,",
                            ));
                        }
                        for _ in 0..slices.len() {
                            e.push('0');
                            e.push(',');
                        }
                        e.pop();
                    }
                    e.push(')');
                    e
                };
                let expr_r = build_channel(&|rgb| rgb.0.to_string());
                let expr_g = build_channel(&|rgb| rgb.1.to_string());
                let expr_b = build_channel(&|rgb| rgb.2.to_string());
                let pie_next = format!("pcp{i}");
                chain.push_str(&format!(
                    "[{cur}]geq=r='{expr_r}':g='{expr_g}':b='{expr_b}':a=255:s={out_w}x{out_h}[{pie_next}];",
                ));
                cur = pie_next;
                // Overlay the pie (drawn at 0,0 of its own {out_w}x{out_h}
                // canvas) back onto the bg at (pie_left_pad, pie_top).
                let overlay_next = format!("pco{i}");
                chain.push_str(&format!(
                    "[{mb}][{cur}]overlay=x={pie_left_pad}:y={pie_top}[{overlay_next}];"
                ));
                cur = overlay_next;
                // Legend on the right side.
                let legend_x = pie_right + 60;
                let legend_fsz = (height / 22).max(20);
                let swatch_w = 24_u32;
                let line_h = legend_fsz + 16;
                for (idx, slice) in slices.iter().enumerate() {
                    let color = slice
                        .color
                        .clone()
                        .unwrap_or_else(|| palette[idx % palette.len()].to_string());
                    let ly = pie_top + (idx as u32) * line_h;
                    let sw_next = format!("pcs{idx}_{i}");
                    chain.push_str(&format!(
                        "[{cur}]drawbox=x={legend_x}:y={ly}:w={swatch_w}:h={swatch_w}:color={color}:t=fill[{sw_next}];"
                    ));
                    cur = sw_next;
                    let pct_str = if total > 0.0 {
                        format!("{:.0}%", slice.percent.max(0.0) / total * 100.0)
                    } else {
                        "0%".to_string()
                    };
                    let lab_text = format!("{} — {}", slice.label, pct_str);
                    let l_esc = escape_drawtext(&lab_text);
                    let lab_x = legend_x + swatch_w + 12;
                    let lab_y = ly + 2;
                    let lab_next = format!("pcl{idx}_{i}");
                    chain.push_str(&format!(
                        "[{cur}]drawtext=text='{l_esc}':fontcolor=white:fontsize={legend_fsz}:\
                         x={lab_x}:y={lab_y}[{lab_next}];"
                    ));
                    cur = lab_next;
                }
                chain.push_str(&format!("[{cur}]copy[{out}];"));
            }
            Scene::EndTag { text, shot, .. } => {
                chain.push_str(&format!(
                    "color=black:s={width}x{height}:r={fps}:d={dur}[bg{i}];"
                ));
                let motion = motion_filter(
                    shot.as_ref().and_then(|m| m.camera_motion.as_deref()),
                    width,
                    height,
                    dur,
                );
                let t = escape_drawtext(text);
                chain.push_str(&format!(
                    "[bg{i}]{motion}drawtext=text='{t}':fontcolor={primary}:fontsize={fsz}:\
                     x=(w-text_w)/2:y=(h-text_h)/2[{out}]",
                    fsz = (height / 8).max(56),
                    primary = brand.primary_color.as_str(),
                ));
            }
            Scene::Comparison {
                title,
                left_label,
                left_value,
                right_label,
                right_value,
                shot,
                ..
            } => {
                // ponytail: black bg + thin vertical divider down the
                // middle. Optional title spans the top. Both halves
                // show a small label above a large value. Layout:
                //   [ title            ]
                //   [ left_label | right_label ]
                //   [ left_value | right_value ]
                //   [    divider         ]
                chain.push_str(&format!(
                    "color=black:s={width}x{height}:r={fps}:d={dur}[bg{i}];"
                ));
                let motion = motion_filter(
                    shot.as_ref().and_then(|m| m.camera_motion.as_deref()),
                    width,
                    height,
                    dur,
                );
                let mut cur = format!("bg{i}");
                let title_offset = if title.is_some() { height / 6 } else { 0 };
                if let Some(t) = title {
                    let t_esc = escape_drawtext(t);
                    let next = format!("ct{i}");
                    chain.push_str(&format!(
                        "[{cur}]{motion}drawtext=text='{t_esc}':fontcolor=white:fontsize={fsz}:\
                         x=(w-text_w)/2:y=60[{next}];",
                        fsz = (height / 14).max(32),
                    ));
                    cur = next;
                }
                // Vertical divider: 2px wide down the middle, from below
                // the title to near the bottom.
                let divider_top = title_offset + 20;
                let divider_h = height.saturating_sub(divider_top + 40);
                let divider_x = (width / 2).saturating_sub(1);
                let divider_next = format!("dv{i}");
                chain.push_str(&format!(
                    "[{cur}]drawbox=x={divider_x}:y={divider_top}:w=2:h={divider_h}:color=#666666:t=fill[{divider_next}];"
                ));
                cur = divider_next;
                // Left label + value, centered in the left half.
                let ll_esc = escape_drawtext(left_label);
                let lv_esc = escape_drawtext(left_value);
                // ponytail: see `escape_drawtext_expr` — the spaces and
                // comma inside the max() must be escaped or ffmpeg's
                // option parser splits on them.
                let left_x = escape_drawtext_expr("max(40, (w/2 - text_w)/2)");
                let label_y = divider_top + 30;
                let value_y = label_y + (height / 5).max(80);
                let next_ll = format!("ll{i}");
                chain.push_str(&format!(
                    "[{cur}]drawtext=text='{ll_esc}':fontcolor=#cccccc:fontsize={fsz}:x={left_x}:y={label_y}[{next_ll}];",
                    fsz = (height / 22).max(22),
                ));
                cur = next_ll;
                let next_lv = format!("lv{i}");
                chain.push_str(&format!(
                    "[{cur}]drawtext=text='{lv_esc}':fontcolor={primary}:fontsize={fsz}:x={left_x}:y={value_y}[{next_lv}];",
                    fsz = (height / 6).max(72),
                    primary = brand.primary_color.as_str(),
                ));
                cur = next_lv;
                // Right label + value, centered in the right half.
                let rl_esc = escape_drawtext(right_label);
                let rv_esc = escape_drawtext(right_value);
                // ponytail: see `left_x`.
                let right_x = escape_drawtext_expr("max(w/2 + 40, w/2 + (w/2 - text_w)/2)");
                let next_rl = format!("rl{i}");
                chain.push_str(&format!(
                    "[{cur}]drawtext=text='{rl_esc}':fontcolor=#cccccc:fontsize={fsz}:x={right_x}:y={label_y}[{next_rl}];",
                    fsz = (height / 22).max(22),
                ));
                cur = next_rl;
                chain.push_str(&format!(
                    "[{cur}]drawtext=text='{rv_esc}':fontcolor=#3aa0ff:fontsize={fsz}:x={right_x}:y={value_y}[{out}];",
                    fsz = (height / 6).max(72),
                ));
            }
            Scene::ProgressBar {
                title,
                progress,
                label,
                shot,
                ..
            } => {
                // ponytail: bg + title (optional) + a track drawbox across
                // the middle, then a filled bar whose width is
                // (progress * track_w). Label sits below the bar.
                //   [ title           ]
                //   [                  ]
                //   [ ███████░░░░░░░░ ]
                //   [    label text     ]
                chain.push_str(&format!(
                    "color=black:s={width}x{height}:r={fps}:d={dur}[bg{i}];"
                ));
                let motion = motion_filter(
                    shot.as_ref().and_then(|m| m.camera_motion.as_deref()),
                    width,
                    height,
                    dur,
                );
                let mut cur = format!("bg{i}");
                let title_y = 60;
                let mut y_cursor = title_y;
                if let Some(t) = title {
                    let t_esc = escape_drawtext(t);
                    let next = format!("pt{i}");
                    chain.push_str(&format!(
                        "[{cur}]{motion}drawtext=text='{t_esc}':fontcolor=white:fontsize={fsz}:\
                         x=(w-text_w)/2:y={title_y}[{next}];",
                        fsz = (height / 14).max(32),
                    ));
                    cur = next;
                    y_cursor += (height / 14).max(32) + 40;
                } else {
                    // Skip the motion header on the bg node directly.
                    let next = format!("pbg{i}");
                    if motion.is_empty() {
                        chain.push_str(&format!("[{cur}]copy[{next}];"));
                    } else {
                        chain.push_str(&format!("[{cur}]{motion}[{next}];"));
                    }
                    cur = next;
                    y_cursor = height / 3;
                }
                // Track + bar geometry. Track sits 60% across the width,
                // centered, with a 28-px height. Filled bar overlays the
                // track from the left with width = progress * track_w.
                let p = progress.clamp(0.0, 1.0);
                let track_w = (width * 3 / 5).max(200);
                let track_x = (width - track_w) / 2;
                let track_h = 28;
                let track_y = y_cursor;
                let track_next = format!("trk{i}");
                chain.push_str(&format!(
                    "[{cur}]drawbox=x={track_x}:y={track_y}:w={track_w}:h={track_h}:\
                     color=#333333:t=fill[{track_next}];"
                ));
                cur = track_next;
                let fill_w = ((track_w as f32) * p).round() as u32;
                if fill_w > 0 {
                    let fill_next = format!("fl{i}");
                    chain.push_str(&format!(
                        "[{cur}]drawbox=x={track_x}:y={track_y}:w={fill_w}:h={track_h}:\
                         color={primary}:t=fill[{fill_next}];",
                        primary = brand.primary_color.as_str(),
                    ));
                    cur = fill_next;
                }
                // Optional label below the bar.
                if let Some(lab) = label {
                    let l_esc = escape_drawtext(lab);
                    chain.push_str(&format!(
                        "[{cur}]drawtext=text='{l_esc}':fontcolor=#cccccc:fontsize={fsz}:\
                         x=(w-text_w)/2:y={label_y}[{out}];",
                        fsz = (height / 22).max(22),
                        label_y = track_y + track_h + 30,
                    ));
                } else {
                    chain.push_str(&format!("[{cur}]copy[{out}];"));
                }
            }
            Scene::Callout {
                title,
                body,
                kind,
                shot,
                ..
            } => {
                // ponytail: bg + a colored accent strip down the left
                // edge + title in white + body text in light gray. The
                // accent color picks itself from `kind`: tip → cyan,
                // warning → orange, info → blue; anything else falls
                // back to brand primary_color.
                let accent = match kind.as_str() {
                    "tip" => "#22d3ee",
                    "warning" => "#ff8c42",
                    "info" => "#3aa0ff",
                    _ => brand.primary_color.as_str(),
                };
                chain.push_str(&format!(
                    "color=black:s={width}x{height}:r={fps}:d={dur}[bg{i}];"
                ));
                let motion = motion_filter(
                    shot.as_ref().and_then(|m| m.camera_motion.as_deref()),
                    width,
                    height,
                    dur,
                );
                let mut cur = format!("bg{i}");
                // Apply motion to bg before drawing on it. When motion
                // is empty, emit a copy pass-through so the `[cb{i}]`
                // pad label exists for the downstream filters. Skipping
                // it entirely leaves the pad unconnected and the next
                // filter (which uses `[cb{i}]` as input) becomes empty,
                // which ffmpeg rejects with "No such filter: ''".
                let mb = format!("cb{i}");
                if motion.is_empty() {
                    chain.push_str(&format!("[{cur}]copy[{mb}];"));
                } else {
                    chain.push_str(&format!("[{cur}]{motion}[{mb}];"));
                }
                cur = mb;
                // Accent strip — 8px wide down the full height on the
                // left edge.
                let strip_w = 8;
                let strip_h = height;
                let acc_next = format!("ac{i}");
                chain.push_str(&format!(
                    "[{cur}]drawbox=x=0:y=0:w={strip_w}:h={strip_h}:color={accent}:t=fill[{acc_next}];"
                ));
                cur = acc_next;
                // Title in white, indented past the strip.
                let t_esc = escape_drawtext(title);
                let title_x = strip_w + 30;
                let title_y = 80;
                let title_next = format!("clt{i}");
                chain.push_str(&format!(
                    "[{cur}]drawtext=text='{t_esc}':fontcolor=white:fontsize={fsz}:x={title_x}:y={title_y}[{title_next}];",
                    fsz = (height / 12).max(40),
                ));
                cur = title_next;
                // Body text below the title, wrapping is not implemented
                // (drawtext has no native wrap); caller is responsible
                // for keeping body lines short.
                let b_esc = escape_drawtext(body);
                let body_y = title_y + (height / 8).max(80);
                chain.push_str(&format!(
                    "[{cur}]drawtext=text='{b_esc}':fontcolor=#cccccc:fontsize={fsz}:x={title_x}:y={body_y}[{out}];",
                    fsz = (height / 22).max(22),
                ));
            }
            Scene::KpiGrid {
                title, cells, shot, ..
            } => {
                // ponytail: bg + title at top, then a grid of cells
                // laid out by ceil(sqrt(n)) columns. Each cell shows the
                // big value in brand primary_color, a small label in
                // #cccccc, and an optional colored arrow + percent for
                // any change. The grid sits in the lower 75% of the frame.
                chain.push_str(&format!(
                    "color=black:s={width}x{height}:r={fps}:d={dur}[bg{i}];"
                ));
                let motion = motion_filter(
                    shot.as_ref().and_then(|m| m.camera_motion.as_deref()),
                    width,
                    height,
                    dur,
                );
                let mut cur = format!("bg{i}");
                let mb = format!("kgb{i}");
                if motion.is_empty() {
                    chain.push_str(&format!("[{cur}]copy[{mb}];"));
                    cur = mb;
                } else {
                    chain.push_str(&format!("[{cur}]{motion}[{mb}];"));
                    cur = mb;
                }
                // Title across the top.
                let t_esc = escape_drawtext(title);
                let title_y = 60;
                let t_next = format!("kgt{i}");
                chain.push_str(&format!(
                    "[{cur}]drawtext=text='{t_esc}':fontcolor=white:fontsize={fsz}:\
                     x=(w-text_w)/2:y={title_y}[{t_next}];",
                    fsz = (height / 14).max(32),
                ));
                cur = t_next;
                // Layout: choose columns = ceil(sqrt(n)). Skip if no cells.
                if !cells.is_empty() {
                    let n = cells.len() as u32;
                    let cols = (n as f64).sqrt().ceil() as u32;
                    let rows = n.div_ceil(cols);
                    let grid_top = title_y + (height / 8).max(80);
                    let grid_h = height.saturating_sub(grid_top + 40);
                    let cell_w = width / cols;
                    let cell_h = grid_h / rows;
                    for (idx, cell) in cells.iter().enumerate() {
                        let col = (idx as u32) % cols;
                        let row = (idx as u32) / cols;
                        let cell_x = col * cell_w;
                        let cell_y = grid_top + row * cell_h;
                        // Value (big, primary color, centered horizontally).
                        let mut v_text = cell.value.clone();
                        if let Some(suf) = &cell.suffix {
                            v_text.push_str(suf);
                        }
                        let v_esc = escape_drawtext(&v_text);
                        let val_next = format!("kv{idx}_{i}");
                        chain.push_str(&format!(
                            "[{cur}]drawtext=text='{v_esc}':fontcolor={primary}:fontsize={fsz}:\
                             x={val_x}:y={val_y}[{val_next}];",
                            fsz = (cell_h / 3).max(48),
                            primary = brand.primary_color.as_str(),
                            val_x = escape_drawtext_expr(&format!(
                                "max(20, {cell_x} + (text_w*-1)/2 + {cell_w}/2)"
                            )),
                            val_y = cell_y + 20,
                        ));
                        cur = val_next;
                        // Label (small, gray, below the value).
                        let l_esc = escape_drawtext(&cell.label);
                        let lab_next = format!("kl{idx}_{i}");
                        chain.push_str(&format!(
                            "[{cur}]drawtext=text='{l_esc}':fontcolor=#cccccc:fontsize={fsz}:\
                             x={lab_x}:y={lab_y}[{lab_next}];",
                            fsz = (cell_h / 9).max(18),
                            lab_x = escape_drawtext_expr(&format!(
                                "max(20, {cell_x} + (text_w*-1)/2 + {cell_w}/2)"
                            )),
                            lab_y = cell_y + 20 + (cell_h / 3).max(48) + 12,
                        ));
                        cur = lab_next;
                        // Optional change arrow + percent.
                        if let Some(pct) = cell.change {
                            let (arrow, color) = if pct >= 0.0 {
                                ("▲ ", "#34d399")
                            } else {
                                ("▼ ", "#ff6b6b")
                            };
                            let abs = pct.abs().round() as u32;
                            let ch_text = format!("{arrow}{abs}%");
                            let ch_esc = escape_drawtext(&ch_text);
                            let ch_next = format!("kc{idx}_{i}");
                            chain.push_str(&format!(
                                "[{cur}]drawtext=text='{ch_esc}':fontcolor={color}:fontsize={fsz}:\
                                 x={ch_x}:y={ch_y}[{ch_next}];",
                                fsz = (cell_h / 9).max(18),
                                ch_x = escape_drawtext_expr(&format!(
                                    "max(20, {cell_x} + (text_w*-1)/2 + {cell_w}/2)"
                                )),
                                ch_y = cell_y
                                    + 20
                                    + (cell_h / 3).max(48)
                                    + 12
                                    + (cell_h / 9).max(18)
                                    + 8,
                            ));
                            cur = ch_next;
                        }
                    }
                }
                // Close out the chain.
                chain.push_str(&format!("[{cur}]copy[{out}];"));
            }
        }
        chains.push(chain);
    }

    // ponytail: if any scene declares a transition, replace the final
    // concat with an xfade chain. Each scene that has shot.transition is
    // extended by duration_s/2 on both sides so the join doesn't shrink
    // total runtime. No transitions → fall back to plain concat (cheap).
    let transitions: Vec<Option<crate::compose::TransitionSpec>> = scenes
        .iter()
        .map(|s| {
            let shot = match s {
                Scene::HeroTitle { shot, .. }
                | Scene::TextCard { shot, .. }
                | Scene::StatCard { shot, .. }
                | Scene::BarChart { shot, .. }
                | Scene::LineChart { shot, .. }
                | Scene::PieChart { shot, .. }
                | Scene::CaptionOverlay { shot, .. }
                | Scene::QuoteCard { shot, .. }
                | Scene::Comparison { shot, .. }
                | Scene::ProgressBar { shot, .. }
                | Scene::Callout { shot, .. }
                | Scene::KpiGrid { shot, .. }
                | Scene::EndTag { shot, .. }
                | Scene::TerminalScene { shot, .. } => shot.as_ref(),
                Scene::ClipCut { shot, .. } => shot.as_ref(),
            };
            shot.and_then(|m| m.transition.as_ref()).cloned()
        })
        .collect();

    let n = chains.len();
    // ponytail: each scene renderer ends its chain with `;` so it
    // concatenates cleanly. Different renderers were inconsistent —
    // some omitted the trailing `;` — which made `chains.join(";")`
    // produce `;;` between certain scenes and an unseparated junction
    // between others. Normalize: every chain ends with exactly one `;`.
    let chains_clean: Vec<String> = chains
        .iter()
        .map(|c| {
            let trimmed = c.trim_end_matches(';');
            format!("{trimmed};")
        })
        .collect();
    // ponytail: each chain already ends with exactly one `;`. Join
    // them with no separator so the result is a clean `...;...;...;`
    // sequence. The concat / xfade tail is appended next — it does
    // NOT get its own leading `;`, since the previous chain's trailing
    // one already separates them. (Earlier versions added a leading
    // `;` here, which produced `;;` once the prior chain was
    // normalised, and ffmpeg rejected the double separator.)
    let mut filter_complex = chains_clean.join("");

    let has_transitions = transitions.iter().any(|t| t.is_some());
    if !has_transitions {
        let concat_inputs: String = (0..n).map(|i| format!("[v{i}]")).collect();
        filter_complex.push_str(&format!("{concat_inputs}concat=n={n}:v=1:a=0[vout]"));
    } else {
        // ponytail: build an xfade chain. Scene i's effective duration is
        // extended by half its transition (so xfade has overlapping frames);
        // scene i+1 starts at offset sum(prev_durs). xfade emits one
        // continuous stream ending in [xf0].
        let fps = comp_fps(fps);
        let scene_durs: Vec<f32> = scenes.iter().map(scene_duration).collect();
        let total: f32 = scene_durs.iter().sum();
        // Compute end times for each xfade boundary.
        // boundary_offset[i] = time at which scene i+1 starts (in the
        // concatenated timeline). Subtract half transition duration.
        let mut boundaries = Vec::with_capacity(n);
        let mut acc = 0.0_f32;
        for i in 0..n {
            let half = transitions[i]
                .as_ref()
                .map(|t| t.duration_s / 2.0)
                .unwrap_or(0.0);
            boundaries.push(acc - half);
            acc += scene_durs[i];
        }
        // First xfade: [v0] [v1] → [xf0], offset = boundaries[1]
        let t1 = transitions[0]
            .as_ref()
            .map(|t| t.kind.clone())
            .unwrap_or_else(|| "fade".into());
        let d1 = transitions[0].as_ref().map(|t| t.duration_s).unwrap_or(0.5);
        filter_complex.push_str(&format!(
            "[v0][v1]xfade=transition={t1}:duration={d1}:offset={o:.3}[xf0];",
            o = boundaries[1].max(0.0),
        ));
        // Chain the rest: [xf_{i-1}][v_{i+1}] → [xf_{i}]
        let mut prev = "xf0".to_string();
        for i in 1..(n - 1) {
            let kind = transitions[i]
                .as_ref()
                .map(|t| t.kind.clone())
                .unwrap_or_else(|| "fade".into());
            let dur = transitions[i].as_ref().map(|t| t.duration_s).unwrap_or(0.5);
            let offset = boundaries[i + 1].max(0.0);
            let next = format!("xf{i}");
            filter_complex.push_str(&format!(
                "[{prev}][v{i_plus}]xfade=transition={kind}:duration={dur}:offset={o:.3}[{next}];",
                i_plus = i + 1,
                o = offset,
            ));
            prev = next;
        }
        // ponytail: clamp the output to `total` so an off-by-one in xfade
        // doesn't bleed past the intended end. Cheap insurance.
        filter_complex.push_str(&format!(
            "[{prev}]trim=duration={total:.3},setpts=PTS-STARTPTS[vout]"
        ));
        // silence unused fps in this branch
        let _ = fps;
    }

    FilterPlan {
        filter_complex,
        inputs,
    }
}

/// ponytail: forward `fps` arg through the function so the transition
/// branch can reference it without restructuring the signature.
fn comp_fps(fps: u32) -> u32 {
    fps
}

fn scene_duration(s: &Scene) -> f32 {
    match s {
        Scene::HeroTitle {
            duration_s,
            shot: _,
            ..
        }
        | Scene::TextCard {
            duration_s,
            shot: _,
            ..
        }
        | Scene::StatCard {
            duration_s,
            shot: _,
            ..
        }
        | Scene::BarChart {
            duration_s,
            shot: _,
            ..
        }
        | Scene::LineChart {
            duration_s,
            shot: _,
            ..
        }
        | Scene::PieChart {
            duration_s,
            shot: _,
            ..
        }
        | Scene::CaptionOverlay {
            duration_s,
            shot: _,
            ..
        }
        | Scene::QuoteCard {
            duration_s,
            shot: _,
            ..
        }
        | Scene::Comparison {
            duration_s,
            shot: _,
            ..
        }
        | Scene::ProgressBar {
            duration_s,
            shot: _,
            ..
        }
        | Scene::Callout {
            duration_s,
            shot: _,
            ..
        }
        | Scene::KpiGrid {
            duration_s,
            shot: _,
            ..
        }
        | Scene::EndTag {
            duration_s,
            shot: _,
            ..
        } => *duration_s,
        Scene::TerminalScene {
            duration_s,
            shot: _,
            ..
        } => *duration_s,
        Scene::ClipCut {
            in_s,
            out_s,
            shot: _,
            ..
        } => (out_s - in_s).max(0.0),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::compose::{KpiCell, LineSeries, PieSlice};

    #[test]
    fn motion_filter_static_is_empty() {
        // ponytail: no motion = no filter chunk; the bg goes straight into
        // drawtext with no extra scale/crop. This keeps static scenes fast.
        let f = motion_filter(None, 1920, 1080, 3.0);
        assert!(
            f.is_empty(),
            "static motion must produce empty filter, got: {f}"
        );
        let f = motion_filter(Some("static"), 1920, 1080, 3.0);
        assert!(
            f.is_empty(),
            "explicit static motion must produce empty filter, got: {f}"
        );
    }

    #[test]
    fn motion_filter_push_scales_and_crops() {
        let f = motion_filter(Some("push"), 1920, 1080, 3.0);
        assert!(
            f.contains("scale=2208:1242"),
            "push should scale to 1.15× (2208x1242 for 1920x1080), got: {f}"
        );
        assert!(
            f.contains("crop=1920:1080"),
            "push should crop back to source size, got: {f}"
        );
    }

    #[test]
    fn motion_filter_pan_uses_time_expression() {
        let f = motion_filter(Some("pan"), 1920, 1080, 4.0);
        assert!(
            f.contains("t/4"),
            "pan should reference scene duration t, got: {f}"
        );
        assert!(
            f.contains("scale=2208:1242"),
            "pan should also scale up, got: {f}"
        );
    }

    #[test]
    fn motion_filter_fade_uses_fade_filter() {
        let f = motion_filter(Some("fade"), 1920, 1080, 3.0);
        assert!(f.contains("fade=t=in"), "fade should fade in, got: {f}");
        assert!(f.contains("fade=t=out"), "fade should fade out, got: {f}");
    }

    #[test]
    fn quote_card_emits_quote_author_and_source_in_filter_graph() {
        // ponytail: end-to-end check that the QuoteCard render chunk emits
        // the quote, the author prefix ("— "), and the source line. Source
        // is optional but here it's present so we can assert all three.
        let scenes = vec![Scene::QuoteCard {
            quote: "Rust makes systems programming fun again.".into(),
            author: Some("Graydon Hoare".into()),
            source: Some("Interview, 2024".into()),
            duration_s: 3.0,
            shot: None,
        }];
        let plan = build_filter_graph(&scenes, 1920, 1080, 30);
        assert!(
            plan.filter_complex
                .contains("Rust makes systems programming fun again"),
            "quote text missing from filter graph:\n{}",
            plan.filter_complex
        );
        assert!(
            plan.filter_complex.contains("Graydon Hoare"),
            "author missing:\n{}",
            plan.filter_complex
        );
        assert!(
            plan.filter_complex.contains(r"Interview\, 2024"),
            "source missing or comma not escaped:\n{}",
            plan.filter_complex
        );
    }

    #[test]
    fn quote_card_without_source_skips_source_line() {
        let scenes = vec![Scene::QuoteCard {
            quote: "Stay hungry, stay foolish.".into(),
            author: Some("Steve Jobs".into()),
            source: None,
            duration_s: 2.0,
            shot: None,
        }];
        let plan = build_filter_graph(&scenes, 1920, 1080, 30);
        assert!(
            plan.filter_complex.contains("Stay hungry"),
            "quote missing:\n{}",
            plan.filter_complex
        );
        assert!(
            plan.filter_complex.contains("Steve Jobs"),
            "author missing:\n{}",
            plan.filter_complex
        );
        // No source → just a copy chunk for the attribution.
        assert!(
            plan.filter_complex.contains("copy"),
            "attribution should fall through copy when no source:\n{}",
            plan.filter_complex
        );
    }

    #[test]
    fn brand_primary_color_overrides_default_accent_in_filter_graph() {
        // ponytail: a custom BrandTheme.primary_color must replace the
        // hardcoded #ffcc00 in StatCard / QuoteCard author / EndTag.
        // EndTag is the simplest scene to assert against (one drawtext).
        let scenes = vec![Scene::EndTag {
            text: "Fin.".into(),
            duration_s: 1.0,
            shot: None,
        }];
        let brand = BrandTheme {
            primary_color: "#00ff88".into(),
            palette: vec![],
        };
        let plan = build_filter_graph_with_brand(&scenes, 1920, 1080, 30, &brand);
        assert!(
            plan.filter_complex.contains("fontcolor=#00ff88"),
            "custom primary_color must appear in filter graph:\n{}",
            plan.filter_complex
        );
        assert!(
            !plan.filter_complex.contains("fontcolor=#ffcc00"),
            "default accent must NOT appear when brand overrides it:\n{}",
            plan.filter_complex
        );
    }

    #[test]
    fn comparison_emits_divider_and_both_halves() {
        // ponytail: end-to-end check that Comparison renders the vertical
        // divider drawbox + both halves' labels and values. Title is
        // optional; this test exercises the no-title path.
        let scenes = vec![Scene::Comparison {
            title: None,
            left_label: "Before".into(),
            left_value: "32s".into(),
            right_label: "After".into(),
            right_value: "12s".into(),
            duration_s: 3.0,
            shot: None,
        }];
        let plan = build_filter_graph(&scenes, 1920, 1080, 30);
        // Vertical divider: a thin drawbox near the middle.
        assert!(
            plan.filter_complex.contains("drawbox=x=959"),
            "expected divider at x=959 (width/2 - 1):\n{}",
            plan.filter_complex
        );
        // Both labels and values appear.
        assert!(
            plan.filter_complex.contains("Before"),
            "left label missing:\n{}",
            plan.filter_complex
        );
        assert!(
            plan.filter_complex.contains("32s"),
            "left value missing:\n{}",
            plan.filter_complex
        );
        assert!(
            plan.filter_complex.contains("After"),
            "right label missing:\n{}",
            plan.filter_complex
        );
        assert!(
            plan.filter_complex.contains("12s"),
            "right value missing:\n{}",
            plan.filter_complex
        );
    }

    #[test]
    fn comparison_with_title_includes_title_at_top() {
        let scenes = vec![Scene::Comparison {
            title: Some("Build time".into()),
            left_label: "Old".into(),
            left_value: "60s".into(),
            right_label: "New".into(),
            right_value: "8s".into(),
            duration_s: 2.5,
            shot: None,
        }];
        let plan = build_filter_graph(&scenes, 1920, 1080, 30);
        assert!(
            plan.filter_complex.contains("Build time"),
            "title missing from filter graph:\n{}",
            plan.filter_complex
        );
    }

    #[test]
    fn progress_bar_emits_track_and_filled_segment_and_label() {
        // ponytail: end-to-end check that ProgressBar renders a track
        // drawbox, a filled drawbox at progress*track_w, and the label.
        let scenes = vec![Scene::ProgressBar {
            title: Some("Coverage".into()),
            progress: 0.5,
            label: Some("halfway there".into()),
            duration_s: 2.0,
            shot: None,
        }];
        let plan = build_filter_graph(&scenes, 1920, 1080, 30);
        assert!(
            plan.filter_complex.contains("Coverage"),
            "title missing:\n{}",
            plan.filter_complex
        );
        // Track color (#333333) appears as the bg bar.
        assert!(
            plan.filter_complex.contains("color=#333333"),
            "expected track bg color:\n{}",
            plan.filter_complex
        );
        // Fill bar color matches the default brand primary_color (#ffcc00).
        assert!(
            plan.filter_complex.contains("color=#ffcc00"),
            "expected primary-color fill:\n{}",
            plan.filter_complex
        );
        assert!(
            plan.filter_complex.contains("halfway there"),
            "label missing:\n{}",
            plan.filter_complex
        );
    }

    #[test]
    fn progress_bar_clamps_to_zero_fill_when_progress_is_zero() {
        // ponytail: progress=0 should emit the track but skip the
        // filled-segment drawbox entirely (fill_w == 0). No spurious
        // color=#ffcc00 should appear.
        let scenes = vec![Scene::ProgressBar {
            title: None,
            progress: 0.0,
            label: None,
            duration_s: 1.0,
            shot: None,
        }];
        let plan = build_filter_graph(&scenes, 1920, 1080, 30);
        assert!(
            plan.filter_complex.contains("color=#333333"),
            "track must still render when progress=0:\n{}",
            plan.filter_complex
        );
        assert!(
            !plan.filter_complex.contains("color=#ffcc00"),
            "fill drawbox must NOT render when progress=0:\n{}",
            plan.filter_complex
        );
    }

    #[test]
    fn callout_emits_title_body_and_tip_accent_strip() {
        // ponytail: end-to-end check that Callout renders title (white),
        // body (#cccccc), and the tip accent strip (#22d3ee).
        let scenes = vec![Scene::Callout {
            title: "Shipping Rule".into(),
            body: "Start with one visible win.".into(),
            kind: "tip".into(),
            duration_s: 2.5,
            shot: None,
        }];
        let plan = build_filter_graph(&scenes, 1920, 1080, 30);
        assert!(
            plan.filter_complex.contains("Shipping Rule"),
            "title missing:\n{}",
            plan.filter_complex
        );
        assert!(
            plan.filter_complex.contains("Start with one visible win."),
            "body missing:\n{}",
            plan.filter_complex
        );
        assert!(
            plan.filter_complex.contains("color=#22d3ee"),
            "expected tip accent color:\n{}",
            plan.filter_complex
        );
        assert!(
            plan.filter_complex.contains("drawbox=x=0:y=0:w=8"),
            "expected 8-px accent strip at x=0:\n{}",
            plan.filter_complex
        );
    }

    #[test]
    fn callout_warning_uses_orange_accent_not_tip_cyan() {
        // ponytail: kind=warning must switch the accent to orange
        // (#ff8c42) and NOT emit the tip cyan (#22d3ee).
        let scenes = vec![Scene::Callout {
            title: "Watch out".into(),
            body: "Don't skip the dry-run.".into(),
            kind: "warning".into(),
            duration_s: 1.5,
            shot: None,
        }];
        let plan = build_filter_graph(&scenes, 1920, 1080, 30);
        assert!(
            plan.filter_complex.contains("color=#ff8c42"),
            "expected warning accent color:\n{}",
            plan.filter_complex
        );
        assert!(
            !plan.filter_complex.contains("color=#22d3ee"),
            "warning must NOT use tip cyan:\n{}",
            plan.filter_complex
        );
    }

    #[test]
    fn kpi_grid_emits_title_and_all_cell_values() {
        // ponytail: 4-cell grid → title + 4 values + 4 labels, all in the
        // filter graph. No change arrows → no ▲/▼ glyphs.
        let scenes = vec![Scene::KpiGrid {
            title: "Release pulse".into(),
            cells: vec![
                KpiCell {
                    label: "PRs".into(),
                    value: "14".into(),
                    change: None,
                    suffix: None,
                },
                KpiCell {
                    label: "Build".into(),
                    value: "11".into(),
                    change: None,
                    suffix: Some(" min".into()),
                },
                KpiCell {
                    label: "Bugs".into(),
                    value: "2".into(),
                    change: None,
                    suffix: None,
                },
                KpiCell {
                    label: "Demos".into(),
                    value: "5".into(),
                    change: None,
                    suffix: None,
                },
            ],
            duration_s: 3.0,
            shot: None,
        }];
        let plan = build_filter_graph(&scenes, 1920, 1080, 30);
        assert!(
            plan.filter_complex.contains("Release pulse"),
            "title missing:\n{}",
            plan.filter_complex
        );
        for v in &["14", "11", "2", "5"] {
            assert!(
                plan.filter_complex.contains(v),
                "value {v} missing:\n{}",
                plan.filter_complex
            );
        }
        for l in &["PRs", "Build", "Bugs", "Demos"] {
            assert!(
                plan.filter_complex.contains(l),
                "label {l} missing:\n{}",
                plan.filter_complex
            );
        }
        assert!(
            !plan.filter_complex.contains("▲"),
            "no change arrows expected:\n{}",
            plan.filter_complex
        );
    }

    #[test]
    fn kpi_grid_change_arrows_use_green_for_positive_red_for_negative() {
        // ponytail: positive change → green ▲, negative → red ▼.
        let scenes = vec![Scene::KpiGrid {
            title: "Pulse".into(),
            cells: vec![
                KpiCell {
                    label: "Up".into(),
                    value: "10".into(),
                    change: Some(18.0),
                    suffix: None,
                },
                KpiCell {
                    label: "Down".into(),
                    value: "3".into(),
                    change: Some(-22.0),
                    suffix: None,
                },
            ],
            duration_s: 1.5,
            shot: None,
        }];
        let plan = build_filter_graph(&scenes, 1920, 1080, 30);
        assert!(
            plan.filter_complex.contains("▲"),
            "positive change should emit up-arrow:\n{}",
            plan.filter_complex
        );
        assert!(
            plan.filter_complex.contains("▼"),
            "negative change should emit down-arrow:\n{}",
            plan.filter_complex
        );
        assert!(
            plan.filter_complex.contains("18"),
            "positive percent missing:\n{}",
            plan.filter_complex
        );
        assert!(
            plan.filter_complex.contains("22"),
            "negative percent missing:\n{}",
            plan.filter_complex
        );
        assert!(
            plan.filter_complex.contains("color=#34d399"),
            "expected green for positive:\n{}",
            plan.filter_complex
        );
        assert!(
            plan.filter_complex.contains("color=#ff6b6b"),
            "expected red for negative:\n{}",
            plan.filter_complex
        );
    }

    #[test]
    fn scene_with_push_camera_motion_emits_scale_crop_in_filter_graph() {
        // ponytail: end-to-end check that the motion filter makes it into
        // the generated filter_complex when a HeroTitle declares push.
        let scenes = vec![Scene::HeroTitle {
            text: "Hi".into(),
            subtitle: None,
            duration_s: 3.0,
            shot: Some(crate::compose::ShotMeta {
                shot_type: None,
                camera_motion: Some("push".into()),
                transition: None,
                narrative_role: None,
            }),
        }];
        let plan = build_filter_graph(&scenes, 1920, 1080, 30);
        assert!(
            plan.filter_complex.contains("scale=2208:1242"),
            "expected 1.15× scale in filter complex:\n{}",
            plan.filter_complex
        );
        assert!(
            plan.filter_complex.contains("crop=1920:1080"),
            "expected crop back to source size:\n{}",
            plan.filter_complex
        );
    }

    #[test]
    fn transitions_replace_concat_with_xfade_chain() {
        // ponytail: when ANY scene declares a transition, the filter graph
        // swaps `concat=n=N:v=1:a=0` for an `xfade` chain. No transitions
        // → fast concat path stays.
        use crate::compose::TransitionSpec;
        let scenes = vec![
            Scene::HeroTitle {
                text: "A".into(),
                subtitle: None,
                duration_s: 3.0,
                shot: Some(crate::compose::ShotMeta {
                    shot_type: None,
                    camera_motion: None,
                    narrative_role: None,
                    transition: Some(TransitionSpec {
                        kind: "fade".into(),
                        duration_s: 0.5,
                    }),
                }),
            },
            Scene::HeroTitle {
                text: "B".into(),
                subtitle: None,
                duration_s: 3.0,
                shot: Some(crate::compose::ShotMeta {
                    shot_type: None,
                    camera_motion: None,
                    narrative_role: None,
                    transition: Some(TransitionSpec {
                        kind: "wipeleft".into(),
                        duration_s: 0.8,
                    }),
                }),
            },
            Scene::HeroTitle {
                text: "C".into(),
                subtitle: None,
                duration_s: 3.0,
                shot: None,
            },
        ];
        let plan = build_filter_graph(&scenes, 1920, 1080, 30);
        assert!(
            !plan.filter_complex.contains("concat=n="),
            "transitions present → must NOT use concat:\n{}",
            plan.filter_complex
        );
        assert!(
            plan.filter_complex
                .contains("xfade=transition=fade:duration=0.5"),
            "first transition should be fade:\n{}",
            plan.filter_complex
        );
        assert!(
            plan.filter_complex
                .contains("xfade=transition=wipeleft:duration=0.8"),
            "second transition should be wipeleft:\n{}",
            plan.filter_complex
        );
        assert!(
            plan.filter_complex.contains("[vout]"),
            "output label must be [vout]:\n{}",
            plan.filter_complex
        );
    }

    #[test]
    fn no_transitions_keeps_concat() {
        // ponytail: when no scene declares a transition, the cheap concat
        // path is used. Tests stay fast.
        let scenes = vec![
            Scene::HeroTitle {
                text: "A".into(),
                subtitle: None,
                duration_s: 2.0,
                shot: None,
            },
            Scene::HeroTitle {
                text: "B".into(),
                subtitle: None,
                duration_s: 2.0,
                shot: None,
            },
        ];
        let plan = build_filter_graph(&scenes, 1920, 1080, 30);
        assert!(
            plan.filter_complex.contains("concat=n=2:v=1:a=0"),
            "no transitions → use concat:\n{}",
            plan.filter_complex
        );
        assert!(
            !plan.filter_complex.contains("xfade="),
            "no transitions → must NOT use xfade:\n{}",
            plan.filter_complex
        );
    }

    #[test]
    fn line_chart_emits_title_rules_and_per_series_segments() {
        // ponytail: regression net for the new LineChart scene. The
        // filter graph must contain the title text, the plot baseline
        // + top rule, every x-axis label, and at least one drawbox
        // per series (one segment per consecutive sample pair).
        let scenes = vec![Scene::LineChart {
            title: "Throughput".into(),
            x_labels: vec!["Q1".into(), "Q2".into(), "Q3".into(), "Q4".into()],
            series: vec![
                LineSeries {
                    label: "Rust".into(),
                    values: vec![0.2, 0.5, 0.7, 0.9],
                    color: None,
                },
                LineSeries {
                    label: "JS".into(),
                    values: vec![0.6, 0.55, 0.5, 0.45],
                    color: Some("#ff5a5a".into()),
                },
            ],
            duration_s: 4.0,
            shot: None,
        }];
        let plan = build_filter_graph(&scenes, 1920, 1080, 30);
        let g = &plan.filter_complex;
        assert!(g.contains("Throughput"), "title missing:\n{g}");
        assert!(
            g.contains("Q1") && g.contains("Q2") && g.contains("Q3") && g.contains("Q4"),
            "all 4 x-axis labels missing:\n{g}"
        );
        // Both series should produce drawbox segments. Each series with
        // 4 samples has 3 segments + 1 end-of-series dot = 4 drawbox
        // calls. Two series → at least 8 `drawbox` invocations in this
        // scene's chain.
        let db_count = g.matches("drawbox=").count();
        assert!(
            db_count >= 8,
            "expected ≥8 drawbox calls (rules + series segments), got {db_count}:\n{g}"
        );
        // The user-supplied color must appear, and so must a default
        // palette color for the series that didn't pick one.
        assert!(
            g.contains("#ff5a5a"),
            "explicit series color must appear in graph:\n{g}"
        );
        assert!(
            g.contains("#3aa0ff"),
            "default palette color must appear for unset series:\n{g}"
        );
    }

    #[test]
    fn line_chart_with_single_series_and_no_x_labels_skips_label_row() {
        // ponytail: degenerate but legal — one series, zero labels. The
        // renderer should still emit at least one segment (sample pair)
        // and not panic on an empty x_labels slice.
        let scenes = vec![Scene::LineChart {
            title: "Solo".into(),
            x_labels: vec![],
            series: vec![LineSeries {
                label: "A".into(),
                values: vec![0.1, 0.9],
                color: None,
            }],
            duration_s: 3.0,
            shot: None,
        }];
        let plan = build_filter_graph(&scenes, 1920, 1080, 30);
        let g = &plan.filter_complex;
        assert!(g.contains("Solo"), "title missing:\n{g}");
        assert!(
            g.contains("drawbox="),
            "at least one segment must be drawn:\n{g}"
        );
    }

    #[test]
    fn pie_chart_emits_geq_pie_overlay_and_legend_with_percentages() {
        // ponytail: the pie chart is the only scene using the geq
        // filter. The graph must contain a geq invocation with the
        // per-slice rgb triples, an overlay call to position the pie
        // on the bg, and a legend with the slice labels + normalized
        // percentages (input 30/40/30 → 30%/40%/30%).
        let scenes = vec![Scene::PieChart {
            title: "Share".into(),
            slices: vec![
                PieSlice {
                    label: "Rust".into(),
                    percent: 30.0,
                    color: Some("#3aa0ff".into()),
                },
                PieSlice {
                    label: "Go".into(),
                    percent: 40.0,
                    color: None,
                },
                PieSlice {
                    label: "JS".into(),
                    percent: 30.0,
                    color: Some("#ff5a5a".into()),
                },
            ],
            duration_s: 4.0,
            shot: None,
        }];
        let plan = build_filter_graph(&scenes, 1920, 1080, 30);
        let g = &plan.filter_complex;
        assert!(g.contains("geq="), "pie must use geq to draw slices:\n{g}");
        assert!(
            g.contains("overlay="),
            "pie must overlay back onto the bg:\n{g}"
        );
        // Legend labels (with em-dash + percent). drawtext escapes `%` → `\%`.
        assert!(
            g.contains("Rust — 30\\%"),
            "rust legend entry missing:\n{g}"
        );
        assert!(g.contains("Go — 40\\%"), "go legend entry missing:\n{g}");
        assert!(g.contains("JS — 30\\%"), "js legend entry missing:\n{g}");
        // The two explicit colors must appear in the rgb triples. Each channel
        // (r/g/b) is a separate expression, so we check the channel
        // values independently rather than looking for a contiguous
        // `r\,g\,b` triple.
        // Rust #3aa0ff → r=58, g=160, b=255. The channel value is sandwiched
        // between FFmpeg-escaped commas, so the literal form is `\,58\,`.
        assert!(g.contains("\\,58\\,"), "rust r missing:\n{g}");
        assert!(g.contains("\\,160\\,"), "rust g missing:\n{g}");
        assert!(g.contains("\\,255\\,"), "rust b missing:\n{g}");
        // JS #ff5a5a → r=255, g=90, b=90. The 255 value collides with
        // rust's b, so just check for the green/blue channel.
        assert!(g.contains("\\,90\\,"), "js g/b missing:\n{g}");
    }

    #[test]
    fn pie_chart_normalizes_arbitrary_percent_inputs() {
        // ponytail: input 3 / 4 / 3 (no percent sign, no /100) must
        // still produce 30%/40%/30% in the legend. Normalization lives
        // at render time so authors can use any unit.
        let scenes = vec![Scene::PieChart {
            title: "Mix".into(),
            slices: vec![
                PieSlice {
                    label: "A".into(),
                    percent: 3.0,
                    color: None,
                },
                PieSlice {
                    label: "B".into(),
                    percent: 4.0,
                    color: None,
                },
                PieSlice {
                    label: "C".into(),
                    percent: 3.0,
                    color: None,
                },
            ],
            duration_s: 3.0,
            shot: None,
        }];
        let plan = build_filter_graph(&scenes, 1920, 1080, 30);
        let g = &plan.filter_complex;
        assert!(g.contains("A — 30\\%"), "A label/percent missing:\n{g}");
        assert!(g.contains("B — 40\\%"), "B label/percent missing:\n{g}");
        assert!(g.contains("C — 30\\%"), "C label/percent missing:\n{g}");
    }

    #[test]
    fn hex_to_rgb_accepts_and_rejects() {
        // ponytail: guard the hex parser used by the pie chart's rgb
        // triple. Accepts #RRGGBB and RRGGBB; rejects anything else
        // (returns 0,0,0 so a bad color renders black, not garbage).
        assert_eq!(hex_to_rgb("#3aa0ff"), (0x3a, 0xa0, 0xff));
        assert_eq!(hex_to_rgb("ffcc00"), (0xff, 0xcc, 0x00));
        assert_eq!(hex_to_rgb("xyz"), (0, 0, 0));
        assert_eq!(hex_to_rgb("#abc"), (0, 0, 0)); // wrong length
        assert_eq!(hex_to_rgb(""), (0, 0, 0));
    }

    #[test]
    fn escape_drawtext_escapes_comma_in_text_arg() {
        // ponytail: regression — drawtext uses `,` as the option/value
        // separator, so a literal comma in `text='...'` must be escaped
        // to `\,` or ffmpeg parses the next token as the next filter.
        // Showed up when the showcase TextCard body listed scene kinds.
        assert_eq!(escape_drawtext("a,b"), r"a\,b");
        assert_eq!(
            escape_drawtext("LineChart, PieChart, end"),
            r"LineChart\, PieChart\, end"
        );
        // Existing escapes still work alongside the new comma one.
        assert_eq!(escape_drawtext("100%"), r"100\%");
        assert_eq!(escape_drawtext("a:b"), r"a\:b");
        assert_eq!(escape_drawtext("it's"), r"it\x27s");
        assert_eq!(escape_drawtext(r"back\slash"), r"back\\slash");
    }

    #[test]
    fn textcard_body_with_commas_does_not_split_filter_graph() {
        // ponytail: regression for the showcase smoke render. Before
        // the comma-escape fix, a body containing `LineChart, PieChart`
        // truncated the filter graph at the comma and ffmpeg rejected
        // the dangling `PieChart` as "No such filter".
        let scenes = vec![Scene::TextCard {
            title: "What's next".into(),
            body: "LineChart, PieChart, and per-stage human approval.".into(),
            duration_s: 4.0,
            shot: None,
        }];
        let plan = build_filter_graph(&scenes, 1920, 1080, 30);
        // No comma inside `text='...'` should survive unescaped — and
        // the whole graph should round-trip through ffmpeg's parser
        // without splitting on it. Smoke-check by counting drawtext
        // text args: exactly one, with escaped commas.
        let text_occurrences = plan
            .filter_complex
            .matches(r"text='LineChart\, PieChart\,")
            .count();
        assert!(
            text_occurrences >= 1,
            "expected escaped comma in TextCard body; got: {}",
            plan.filter_complex
        );
    }

    #[test]
    fn terminal_scene_emits_chrome_and_typed_lines() {
        // ponytail: TerminalScene is the synthetic animation scene.
        // Smoke-test: a scene with a Cmd + Out step + a Pill produces a
        // filter graph containing the chrome box, the prompt+cmd text,
        // and the pill drawbox. No `;;` (chain separator collision).
        let scenes = vec![Scene::TerminalScene {
            title: Some("Build log".into()),
            prompt: "$ ".into(),
            accent_color: Some("#00ff88".into()),
            steps: vec![
                TerminalStep::Cmd {
                    text: "cargo build --release".into(),
                    type_speed: 0.02,
                    hold_s: 0.2,
                },
                TerminalStep::Out {
                    text: "Finished release [optimized] target(s)".into(),
                    hold_s: 0.4,
                },
                TerminalStep::Pill {
                    text: "OK".into(),
                    color: Some("#27c93f".into()),
                    hold_s: 1.0,
                },
            ],
            duration_s: 4.0,
            shot: None,
        }];
        let plan = build_filter_graph(&scenes, 1920, 1080, 30);
        assert!(
            plan.filter_complex.contains("color=#1e1e22"),
            "missing chrome drawbox: {}",
            plan.filter_complex
        );
        assert!(
            plan.filter_complex.contains(r"cargo build --release"),
            "missing typed cmd in output: {}",
            plan.filter_complex
        );
        assert!(
            plan.filter_complex.contains(r"Finished release"),
            "missing program output line: {}",
            plan.filter_complex
        );
        assert!(
            plan.filter_complex.contains("color=#27c93f"),
            "missing pill color: {}",
            plan.filter_complex
        );
        assert!(
            !plan.filter_complex.contains(";;"),
            "double chain separator in terminal scene graph"
        );
    }

    #[test]
    fn terminal_scene_truncates_overflow_lines() {
        // ponytail: very long log buffers get truncated to fit the
        // window. Confirm the truncation marker appears when steps
        // exceed available body rows.
        let steps: Vec<TerminalStep> = (0..120)
            .map(|i| TerminalStep::Out {
                text: format!("line {i}"),
                hold_s: 0.1,
            })
            .collect();
        let scenes = vec![Scene::TerminalScene {
            title: None,
            prompt: "$ ".into(),
            accent_color: None,
            steps,
            duration_s: 30.0,
            shot: None,
        }];
        let plan = build_filter_graph(&scenes, 1920, 1080, 30);
        assert!(
            plan.filter_complex.contains(r"more lines"),
            "expected truncation marker for overflow buffer: {}",
            plan.filter_complex
        );
        assert!(
            !plan.filter_complex.contains("line 119"),
            "lines past truncation window should not appear"
        );
    }

    #[test]
    fn terminal_scene_duration_sums_step_timeline() {
        // ponytail: TerminalScene duration is derived from step
        // cursor_duration_s, not from the duration_s field.
        use crate::compose::scene_duration_s;
        let scene = Scene::TerminalScene {
            title: None,
            prompt: "$ ".into(),
            accent_color: None,
            steps: vec![
                TerminalStep::Cmd {
                    text: "ls".into(),
                    type_speed: 0.1,
                    hold_s: 0.5,
                },
                TerminalStep::Out {
                    text: "a.txt".into(),
                    hold_s: 0.4,
                },
                TerminalStep::Pause { seconds: 1.5 },
                TerminalStep::Pill {
                    text: "ready".into(),
                    color: None,
                    hold_s: 5.0,
                },
            ],
            duration_s: 99.0, // should be ignored
            shot: None,
        };
        // ls: 2 chars * 0.1 + 0.5 = 0.7
        // out: 0.4
        // pause: 1.5
        // pill: 0.0 (non-blocking)
        // total: 2.6
        let dur = scene_duration_s(&scene);
        assert!((dur - 2.6).abs() < 0.001, "expected 2.6 (steps), got {dur}");
    }
}
