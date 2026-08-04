//! Composition DSL — turns a `Vec<Scene>` into an FFmpeg filter graph
//! and renders to MP4.
//!
//! ponytail: this exists — scenes are a tagged enum so render code is one
//! match. Add a new scene type only when you can name the FFmpeg filter
//! that draws it.

use std::fmt::Write as _;
use std::path::PathBuf;

use serde::{Deserialize, Serialize};

pub mod brand;
pub mod filter_graph;
pub mod media_profiles;
pub mod render;

pub use brand::BrandTheme;
pub use filter_graph::{build_filter_graph, build_filter_graph_with_brand};
pub use media_profiles::{
    apply_to_composition, get_profile, get_profiles_for_platform, AspectRatio, MediaProfile,
    ALL_PROFILES,
};
pub use render::render_composition;

/// Shot-level metadata. Optional everywhere so existing compositions keep
/// parsing. `camera_motion: static` feeds the slideshow risk gate (static
/// scenes push the risk up).
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct ShotMeta {
    /// `wide` | `medium` | `close` | `insert` | `cutaway` — informational,
    /// not yet rendered.
    #[serde(default)]
    pub shot_type: Option<String>,
    /// `static` | `push` | `pan` | `tilt` | `dolly` — `static` penalizes risk.
    #[serde(default)]
    pub camera_motion: Option<String>,
    /// `setup` | `develop` | `payoff` | `transition` | `bookend` — narrative
    /// arc tracking. Not yet enforced.
    #[serde(default)]
    pub narrative_role: Option<String>,
    /// Cross-fade into the NEXT scene. `kind: fade | wipeleft | wiperight |
    /// slideup | slidedown | circleopen | dissolve`. `duration_s` is the
    /// overlap; both scenes are extended by half so output duration is
    /// preserved. Absent = hard cut.
    #[serde(default)]
    pub transition: Option<TransitionSpec>,
}

/// ponytail: per-scene cross-fade spec. Lives on ShotMeta so authoring a
/// scene plan doesn't need a second field next to it.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct TransitionSpec {
    /// `fade` (default) | `wipeleft` | `wiperight` | `slideup` | `slidedown`
    /// | `circleopen` | `dissolve` — see ffmpeg `xfade` filter docs.
    #[serde(default = "default_transition_kind")]
    pub kind: String,
    #[serde(default = "default_transition_dur")]
    pub duration_s: f32,
}

fn default_transition_kind() -> String {
    "fade".into()
}
fn default_transition_dur() -> f32 {
    0.5
}

impl Default for TransitionSpec {
    fn default() -> Self {
        Self {
            kind: default_transition_kind(),
            duration_s: default_transition_dur(),
        }
    }
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(tag = "type", rename_all = "snake_case")]
pub enum Scene {
    /// Black bg + large centered title (and optional subtitle).
    HeroTitle {
        text: String,
        #[serde(default)]
        subtitle: Option<String>,
        duration_s: f32,
        #[serde(default)]
        shot: Option<ShotMeta>,
    },
    /// Title + body paragraph.
    TextCard {
        title: String,
        body: String,
        duration_s: f32,
        #[serde(default)]
        shot: Option<ShotMeta>,
    },
    /// Big number with a label below.
    StatCard {
        number: String,
        label: String,
        duration_s: f32,
        #[serde(default)]
        shot: Option<ShotMeta>,
    },
    /// Bar chart drawn with `drawbox` filters. Values are normalized 0..1.
    BarChart {
        title: String,
        bars: Vec<Bar>,
        duration_s: f32,
        #[serde(default)]
        shot: Option<ShotMeta>,
    },
    /// Line chart — title across the top, x-axis labels at the bottom,
    /// one or more polylines drawn with `drawbox` segments per series.
    /// Values are 0..=1; the y-range is fixed 0..1 so two series are
    /// visually comparable. Up to 4 series by default; the renderer
    /// picks a palette color per series when none is supplied.
    LineChart {
        title: String,
        x_labels: Vec<String>,
        series: Vec<LineSeries>,
        duration_s: f32,
        #[serde(default)]
        shot: Option<ShotMeta>,
    },
    /// Pie chart — title across the top, a single pie in the middle,
    /// a legend on the right. Slices are drawn with the `geq` filter
    /// computing arc membership from polar coordinates. Percentages
    /// are normalized at render time so authoring in any unit works.
    PieChart {
        title: String,
        slices: Vec<PieSlice>,
        duration_s: f32,
        #[serde(default)]
        shot: Option<ShotMeta>,
    },
    /// Lower-third captions (one line fading through).
    CaptionOverlay {
        lines: Vec<String>,
        duration_s: f32,
        #[serde(default)]
        shot: Option<ShotMeta>,
    },
    /// Pull-quote with attribution. Quote fills the upper half, author
    ///     + optional source sit below in smaller text. Common in explainer
    ///     videos for citing experts / reviewers / data sources.
    QuoteCard {
        quote: String,
        #[serde(default)]
        author: Option<String>,
        #[serde(default)]
        source: Option<String>,
        duration_s: f32,
        #[serde(default)]
        shot: Option<ShotMeta>,
    },
    /// Side-by-side A vs B. Black bg + vertical divider down the
    /// middle; (left_label, left_value) on the left, (right_label,
    /// right_value) on the right. Optional title across the top.
    /// Common pattern: "Before / After", "X vs Y".
    Comparison {
        #[serde(default)]
        title: Option<String>,
        left_label: String,
        left_value: String,
        right_label: String,
        right_value: String,
        duration_s: f32,
        #[serde(default)]
        shot: Option<ShotMeta>,
    },
    /// Horizontal progress bar. Title across the top, a filled bar
    /// whose width is `progress * available_width` (0..=1), and a
    /// label line below (e.g. "82% of the release path is scripted").
    /// Common pattern: completion %, growth indicators, milestone countdowns.
    ProgressBar {
        #[serde(default)]
        title: Option<String>,
        /// 0.0..=1.0 — clamped at render time.
        progress: f32,
        #[serde(default)]
        label: Option<String>,
        duration_s: f32,
        #[serde(default)]
        shot: Option<ShotMeta>,
    },
    /// Highlighted text card with a colored accent strip down the left
    /// edge and a stylized kind (tip | warning | info). Title sits at
    /// the top in white; body text wraps beneath it. Common pattern:
    /// shipping rules, watch-outs, helpful asides.
    Callout {
        title: String,
        body: String,
        /// `tip` (cyan, default) | `warning` (orange) | `info` (blue).
        /// Picks the accent strip color; falls back to brand primary_color
        /// if unknown.
        #[serde(default = "default_callout_kind")]
        kind: String,
        duration_s: f32,
        #[serde(default)]
        shot: Option<ShotMeta>,
    },
    /// A grid of stat cells, each with a big number and a small label.
    /// Layout is auto-picked from cell count (1 row for 1-3 cells,
    /// 2 rows for 4-6, 3 rows for 7-9, etc.). Optional `change` shows
    /// a green up-arrow or red down-arrow with the percent.
    KpiGrid {
        title: String,
        cells: Vec<KpiCell>,
        duration_s: f32,
        #[serde(default)]
        shot: Option<ShotMeta>,
    },
    /// A pre-rendered source clip (no overlay).
    ClipCut {
        src: PathBuf,
        in_s: f32,
        out_s: f32,
        #[serde(default)]
        shot: Option<ShotMeta>,
    },
    /// Synthetic terminal animation — a fake terminal window with a
    /// title bar, a prompt, and a step list. Steps are pure data: no
    /// real capture needed. Common pattern: install walkthroughs,
    /// `git clone` flows, CLI demos where commands and outputs are
    /// predictable.
    ///
    /// `steps` is a vector of `TerminalStep`. Each step is one of:
    /// - `Cmd { text, type_speed?, hold_s? }` — prints the prompt and
    ///   types `text`. `type_speed` defaults to 0.035s/char, `hold_s`
    ///   defaults to 0.3.
    /// - `Out { text, hold_s? }` — a line of program output, revealed
    ///   instantly. `hold_s` defaults to 0.6.
    /// - `Pause { seconds }` — dead time, terminal holds last state.
    ///   Use to sync with narration.
    /// - `Pill { text, color?, hold_s? }` — non-blocking floating
    ///   badge in the top-right. Doesn't advance the cursor.
    ///
    /// Total scene duration is derived from the steps, so the
    /// `duration_s` field on the scene is informational only — the
    /// renderer ignores it and uses the step timeline.
    TerminalScene {
        #[serde(default)]
        title: Option<String>,
        #[serde(default = "default_prompt")]
        prompt: String,
        #[serde(default)]
        accent_color: Option<String>,
        steps: Vec<TerminalStep>,
        duration_s: f32,
        #[serde(default)]
        shot: Option<ShotMeta>,
    },
    /// End slate / brand tag.
    EndTag {
        text: String,
        duration_s: f32,
        #[serde(default)]
        shot: Option<ShotMeta>,
    },
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Bar {
    pub label: String,
    /// 0.0..=1.0
    pub value: f32,
    /// hex like "#ffcc00"
    #[serde(default = "default_color")]
    pub color: String,
}

/// One series on a `LineChart`. Values are normalized 0..=1 and the
/// renderer picks distinct palette colors when `color` is absent.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct LineSeries {
    pub label: String,
    pub values: Vec<f32>,
    /// Optional hex like "#3aa0ff". Default: brand primary_color.
    #[serde(default)]
    pub color: Option<String>,
}

/// One slice of a `PieChart`. `percent` is normalized to sum to 100
/// at render time (so authoring in any unit works — 30/40/30, 0.3/0.4/0.3,
/// 3/4/3 all produce the same pie).
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct PieSlice {
    pub label: String,
    /// Percent share. Normalized at render time.
    pub percent: f32,
    /// Optional hex like "#ff5a5a". Default: brand primary_color.
    #[serde(default)]
    pub color: Option<String>,
}

/// One cell in a `KpiGrid`. Big number + small label, optional
/// change (percent) and suffix (e.g. " min", "%"). Positive change →
/// green up-arrow, negative → red down-arrow.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct KpiCell {
    pub label: String,
    /// String so callers can pass "82%", "1.2k", etc. Rendered as-is.
    pub value: String,
    /// Optional percent change shown as a colored arrow.
    #[serde(default)]
    pub change: Option<f32>,
    /// Optional suffix appended to the value (e.g. " min").
    #[serde(default)]
    pub suffix: Option<String>,
}

fn default_color() -> String {
    "#3aa0ff".into()
}

fn default_callout_kind() -> String {
    "tip".into()
}

fn default_prompt() -> String {
    "$ ".into()
}

fn default_terminal_type_speed() -> f32 {
    0.035
}
fn default_terminal_cmd_hold() -> f32 {
    0.3
}
fn default_terminal_out_hold() -> f32 {
    0.6
}
fn default_terminal_pill_hold() -> f32 {
    1.6
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Composition {
    pub width: u32,
    pub height: u32,
    pub fps: u32,
    pub scenes: Vec<Scene>,
    /// Optional background audio. Absent = no audio stream in the output.
    #[serde(default)]
    pub audio: Option<AudioSpec>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(tag = "kind", rename_all = "snake_case")]
pub enum AudioSpec {
    /// `anullsrc` — silent stereo at 44.1 kHz, length auto-padded to video.
    Silent,
    /// `sine` tone at the given frequency, length auto-padded.
    Tone { freq_hz: u32 },
    /// Pre-rendered voiceover file from the Narration stage. Looped to the
    /// video length when shorter (background music only). When `duck_under`
    /// is true and there's a second audio input, the renderer lowers the
    /// background while voice is present.
    Narration {
        path: PathBuf,
        #[serde(default)]
        duck_under: bool,
    },
}

impl Composition {
    pub fn total_duration_s(&self) -> f32 {
        self.scenes.iter().map(scene_duration_s).sum()
    }
}

/// One step in a `TerminalScene` step list. The variant drives how
/// the renderer advances the timeline:
/// - `Cmd` types the prompt + text at `type_speed` seconds per char,
///   then holds for `hold_s`.
/// - `Out` reveals a line of program output, then holds for `hold_s`.
/// - `Pause` is dead time (e.g. for narration sync).
/// - `Pill` is a non-blocking floating badge — the cursor advances
///   independently while the pill is on screen.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(tag = "kind", rename_all = "snake_case")]
pub enum TerminalStep {
    Cmd {
        text: String,
        #[serde(default = "default_terminal_type_speed")]
        type_speed: f32,
        #[serde(default = "default_terminal_cmd_hold")]
        hold_s: f32,
    },
    Out {
        text: String,
        #[serde(default = "default_terminal_out_hold")]
        hold_s: f32,
    },
    Pause {
        seconds: f32,
    },
    Pill {
        text: String,
        #[serde(default)]
        color: Option<String>,
        #[serde(default = "default_terminal_pill_hold")]
        hold_s: f32,
    },
}

impl TerminalStep {
    /// ponytail: time this step adds to the terminal's timeline.
    /// Pills don't advance the cursor — they overlap — so the helper
    /// returns the duration the *cursor* spends in this step. The
    /// renderer tracks pills on a separate clock.
    pub fn cursor_duration_s(&self) -> f32 {
        match self {
            TerminalStep::Cmd {
                text,
                type_speed,
                hold_s,
            } => {
                let n = text.chars().count() as f32;
                n * type_speed + hold_s
            }
            TerminalStep::Out { hold_s, .. } => *hold_s,
            TerminalStep::Pause { seconds } => *seconds,
            // ponytail: pills are non-blocking; the cursor advances
            // zero time. The pill is shown for `hold_s` but the next
            // step runs in parallel.
            TerminalStep::Pill { .. } => 0.0,
        }
    }
}

pub fn scene_duration_s(s: &Scene) -> f32 {
    match s {
        Scene::HeroTitle { duration_s, .. }
        | Scene::TextCard { duration_s, .. }
        | Scene::StatCard { duration_s, .. }
        | Scene::BarChart { duration_s, .. }
        | Scene::LineChart { duration_s, .. }
        | Scene::PieChart { duration_s, .. }
        | Scene::CaptionOverlay { duration_s, .. }
        | Scene::QuoteCard { duration_s, .. }
        | Scene::Comparison { duration_s, .. }
        | Scene::ProgressBar { duration_s, .. }
        | Scene::Callout { duration_s, .. }
        | Scene::KpiGrid { duration_s, .. }
        | Scene::EndTag { duration_s, .. } => *duration_s,
        Scene::ClipCut { in_s, out_s, .. } => (out_s - in_s).max(0.0),
        // ponytail: terminal scenes compute their own timeline from
        // the step list (cursor_duration_s sums Cmd/Out/Pause; Pills
        // are non-blocking). The authored `duration_s` is informational
        // — the renderer uses the computed value so a slow pause
        // doesn't truncate.
        Scene::TerminalScene { steps, .. } => {
            steps.iter().map(TerminalStep::cursor_duration_s).sum()
        }
    }
}

/// Stable tag for `Scene` — used by slideshow risk + decision logs.
pub fn scene_kind_tag(s: &Scene) -> &'static str {
    match s {
        Scene::HeroTitle { .. } => "hero_title",
        Scene::TextCard { .. } => "text_card",
        Scene::StatCard { .. } => "stat_card",
        Scene::BarChart { .. } => "bar_chart",
        Scene::LineChart { .. } => "line_chart",
        Scene::PieChart { .. } => "pie_chart",
        Scene::CaptionOverlay { .. } => "caption_overlay",
        Scene::QuoteCard { .. } => "quote_card",
        Scene::Comparison { .. } => "comparison",
        Scene::ProgressBar { .. } => "progress_bar",
        Scene::Callout { .. } => "callout",
        Scene::KpiGrid { .. } => "kpi_grid",
        Scene::ClipCut { .. } => "clip_cut",
        Scene::TerminalScene { .. } => "terminal_scene",
        Scene::EndTag { .. } => "end_tag",
    }
}

/// Build an SRT sidecar from every `CaptionOverlay` scene. Each line gets
/// an even slice of the scene's duration as a single subtitle entry;
/// timestamps are accumulated from preceding scene durations so the SRT
/// lines up with the rendered video timeline.
///
/// ponytail: SRT requires CRLF line endings per spec. We use plain `\n`
/// here — players and ffmpeg's `subtitles=` filter read the file via
/// libavformat which is forgiving. CRLF is the upgrade if a strict
/// consumer ever appears.
pub fn caption_overlay_srt(scenes: &[Scene]) -> String {
    let mut out = String::new();
    let mut entry: u32 = 1;
    let mut t: f32 = 0.0;
    for s in scenes {
        let dur = scene_duration_s(s);
        if let Scene::CaptionOverlay { lines, .. } = s {
            let per = if lines.is_empty() {
                dur
            } else {
                dur / lines.len() as f32
            };
            let scene_start = t;
            for (i, line) in lines.iter().enumerate() {
                let start = scene_start + per * i as f32;
                let end = scene_start + per * (i + 1) as f32;
                let _ = writeln!(out, "{entry}");
                let _ = writeln!(out, "{} --> {}", srt_ts(start), srt_ts(end));
                let _ = writeln!(out, "{}", line.trim());
                let _ = writeln!(out);
                entry += 1;
            }
        }
        t += dur;
    }
    out
}

fn srt_ts(secs: f32) -> String {
    let total_ms = (secs.max(0.0) * 1000.0).round() as u64;
    let h = total_ms / 3_600_000;
    let m = (total_ms % 3_600_000) / 60_000;
    let s = (total_ms % 60_000) / 1_000;
    let ms = total_ms % 1_000;
    format!("{h:02}:{m:02}:{s:02},{ms:03}")
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn shot_meta_round_trips_through_composition() {
        let json = serde_json::json!({
            "type": "hero_title",
            "text": "Hi",
            "duration_s": 2.0,
            "shot": {
                "shot_type": "wide",
                "camera_motion": "push",
                "narrative_role": "setup",
            }
        });
        let s: Scene = serde_json::from_value(json).unwrap();
        match s {
            Scene::HeroTitle {
                shot: ref shot_meta,
                ..
            } => {
                let m = shot_meta.as_ref().expect("shot missing");
                assert_eq!(m.shot_type.as_deref(), Some("wide"));
                assert_eq!(m.camera_motion.as_deref(), Some("push"));
                assert_eq!(m.narrative_role.as_deref(), Some("setup"));
            }
            _ => panic!("wrong variant"),
        }
        // round-trip
        let back = serde_json::to_value(&s).unwrap();
        assert_eq!(back["shot"]["camera_motion"], "push");
    }

    #[test]
    fn shot_meta_optional_legacy_compositions_still_parse() {
        let json = serde_json::json!({
            "type": "hero_title",
            "text": "Legacy",
            "duration_s": 2.0
        });
        let s: Scene = serde_json::from_value(json).unwrap();
        match s {
            Scene::HeroTitle {
                shot: ref shot_meta,
                ..
            } => assert!(shot_meta.is_none()),
            _ => panic!("wrong variant"),
        }
    }

    #[test]
    fn srt_ts_formats_hours_minutes_seconds_milliseconds() {
        assert_eq!(srt_ts(0.0), "00:00:00,000");
        assert_eq!(srt_ts(1.5), "00:00:01,500");
        assert_eq!(srt_ts(61.25), "00:01:01,250");
        assert_eq!(srt_ts(3661.999), "01:01:01,999");
    }

    #[test]
    fn caption_overlay_srt_emits_entries_per_line_per_scene() {
        let scenes = vec![
            Scene::HeroTitle {
                text: "A".into(),
                subtitle: None,
                duration_s: 1.0,
                shot: None,
            },
            Scene::CaptionOverlay {
                lines: vec!["one".into(), "two".into()],
                duration_s: 2.0,
                shot: None,
            },
            Scene::CaptionOverlay {
                lines: vec!["three".into()],
                duration_s: 1.0,
                shot: None,
            },
        ];
        let srt = caption_overlay_srt(&scenes);
        // 3 entries (1+2 from second scene, 1 from third).
        let lines: Vec<&str> = srt.lines().collect();
        assert!(lines.contains(&"1"));
        assert!(lines.contains(&"3"));
        assert!(srt.contains("00:00:01,000 --> 00:00:02,000"));
        assert!(srt.contains("00:00:02,000 --> 00:00:03,000"));
        assert!(srt.contains("00:00:03,000 --> 00:00:04,000"));
        assert!(srt.contains("one"));
        assert!(srt.contains("two"));
        assert!(srt.contains("three"));
    }

    #[test]
    fn caption_overlay_srt_is_empty_when_no_caption_scenes() {
        let scenes = vec![
            Scene::HeroTitle {
                text: "A".into(),
                subtitle: None,
                duration_s: 1.0,
                shot: None,
            },
            Scene::EndTag {
                text: "bye".into(),
                duration_s: 1.0,
                shot: None,
            },
        ];
        assert!(caption_overlay_srt(&scenes).is_empty());
    }
}
