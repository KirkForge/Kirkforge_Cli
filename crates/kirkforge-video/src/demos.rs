//! Zero-API-key demos. Each demo is a JSON spec → `Composition` → FFmpeg.

use std::path::PathBuf;

use crate::compose::{
    render_composition, AudioSpec, Bar, Composition, KpiCell, Scene, TerminalStep,
};

pub struct DemoSpec {
    pub label: &'static str,
    pub description: &'static str,
    pub build: fn() -> Composition,
}

pub fn list() -> &'static [DemoSpec] {
    &[
        DemoSpec {
            label: "world-in-numbers",
            description: "Global-scale story with hero title, stats, bar chart",
            build: world_in_numbers,
        },
        DemoSpec {
            label: "code-to-screen",
            description: "Developer workflow with text cards, comparison, captions",
            build: code_to_screen,
        },
        DemoSpec {
            label: "focusflow-pitch",
            description: "Startup pitch — stat cards + end tag",
            build: focusflow_pitch,
        },
        DemoSpec {
            label: "showcase",
            description: "All 11 scene types in one composition — hero, stat, quote, comparison, progress, callout, kpi grid, bar chart, captions, text card, end tag",
            build: showcase,
        },
    ]
}

pub async fn render(name: &str, out: PathBuf) -> anyhow::Result<PathBuf> {
    let spec = list()
        .iter()
        .find(|d| d.label == name)
        .ok_or_else(|| anyhow::anyhow!("unknown demo: {name}"))?;
    let comp = (spec.build)();
    render_composition(&comp, &out).await?;
    Ok(out)
}

fn world_in_numbers() -> Composition {
    Composition {
        width: 1920,
        height: 1080,
        fps: 30,
        audio: Some(AudioSpec::Tone { freq_hz: 220 }),
        scenes: vec![
            Scene::HeroTitle {
                text: "The World in Numbers".into(),
                subtitle: Some("A 30-second tour".into()),
                duration_s: 4.0,
                shot: None,
            },
            Scene::StatCard {
                number: "8.1B".into(),
                label: "people on Earth".into(),
                duration_s: 3.0,
                shot: None,
            },
            Scene::BarChart {
                title: "Internet penetration by region".into(),
                duration_s: 8.0,
                bars: vec![
                    Bar {
                        label: "N.America".into(),
                        value: 0.93,
                        color: "#3aa0ff".into(),
                    },
                    Bar {
                        label: "Europe".into(),
                        value: 0.89,
                        color: "#ffcc00".into(),
                    },
                    Bar {
                        label: "Asia".into(),
                        value: 0.70,
                        color: "#ff5a5a".into(),
                    },
                    Bar {
                        label: "Africa".into(),
                        value: 0.43,
                        color: "#6cd07a".into(),
                    },
                ],
                shot: None,
            },
            Scene::StatCard {
                number: "67%".into(),
                label: "mobile-first users".into(),
                duration_s: 3.0,
                shot: None,
            },
            Scene::CaptionOverlay {
                lines: vec![
                    "5G growing".into(),
                    "AI accelerating".into(),
                    "video exploding".into(),
                ],
                duration_s: 6.0,
                shot: None,
            },
            Scene::EndTag {
                text: "kirkforge.video".into(),
                duration_s: 3.0,
                shot: None,
            },
        ],
    }
}

fn code_to_screen() -> Composition {
    Composition {
        width: 1920,
        height: 1080,
        fps: 30,
        audio: Some(AudioSpec::Silent),
        scenes: vec![
            Scene::HeroTitle {
                text: "From Code to Screen".into(),
                subtitle: Some("The 5-step pipeline".into()),
                duration_s: 4.0,
                shot: None,
            },
            Scene::TextCard {
                title: "1. Brief".into(),
                body: "User describes the video in plain language.".into(),
                duration_s: 4.0,
                shot: None,
            },
            Scene::TextCard {
                title: "2. Scene Plan".into(),
                body: "Agent composes a shot list with shot_language + narrative_role.".into(),
                duration_s: 5.0,
                shot: None,
            },
            Scene::TextCard {
                title: "3. Render".into(),
                body: "FFmpeg draws text + charts + transitions into a single MP4.".into(),
                duration_s: 5.0,
                shot: None,
            },
            Scene::CaptionOverlay {
                lines: vec!["agentic".into(), "type-safe".into(), "FFmpeg-native".into()],
                duration_s: 5.0,
                shot: None,
            },
            Scene::EndTag {
                text: "rust + ffmpeg".into(),
                duration_s: 2.0,
                shot: None,
            },
        ],
    }
}

fn focusflow_pitch() -> Composition {
    Composition {
        width: 1920,
        height: 1080,
        fps: 30,
        audio: Some(AudioSpec::Tone { freq_hz: 330 }),
        scenes: vec![
            Scene::HeroTitle {
                text: "FocusFlow".into(),
                subtitle: Some("Distraction-free deep work, on demand".into()),
                duration_s: 4.0,
                shot: None,
            },
            Scene::StatCard {
                number: "3.2×".into(),
                label: "more output per hour".into(),
                duration_s: 4.0,
                shot: None,
            },
            Scene::StatCard {
                number: "−47%".into(),
                label: "context switches".into(),
                duration_s: 4.0,
                shot: None,
            },
            Scene::BarChart {
                title: "Time reclaimed per day".into(),
                duration_s: 6.0,
                bars: vec![
                    Bar {
                        label: "Engineers".into(),
                        value: 0.85,
                        color: "#3aa0ff".into(),
                    },
                    Bar {
                        label: "Writers".into(),
                        value: 0.62,
                        color: "#ffcc00".into(),
                    },
                    Bar {
                        label: "Designers".into(),
                        value: 0.55,
                        color: "#6cd07a".into(),
                    },
                ],
                shot: None,
            },
            Scene::EndTag {
                text: "focusflow.app".into(),
                duration_s: 3.0,
                shot: None,
            },
        ],
    }
}

/// `showcase` — every scene type in one composition. Targets ~70s.
/// Useful as a single video to verify the whole DSL renders, and as
/// a screen-test for users who want to see all 11 scene types at
/// once.
fn showcase() -> Composition {
    Composition {
        width: 1920,
        height: 1080,
        fps: 30,
        audio: Some(AudioSpec::Tone { freq_hz: 220 }),
        scenes: vec![
            Scene::HeroTitle {
                text: "KirkForge Showcase".into(),
                subtitle: Some("All 11 scene types in 70 seconds".into()),
                duration_s: 4.0,
                shot: None,
            },
            Scene::StatCard {
                number: "11".into(),
                label: "scene types in the DSL".into(),
                duration_s: 3.0,
                shot: None,
            },
            Scene::QuoteCard {
                quote: "Make the easy things easy and the hard things possible.".into(),
                author: Some("Larry Wall".into()),
                source: Some("Programming is the art of doing one thing at a time.".into()),
                duration_s: 4.0,
                shot: None,
            },
            Scene::Comparison {
                title: Some("Render path".into()),
                left_label: "JS pipeline".into(),
                left_value: "9 tools".into(),
                right_label: "FFmpeg-native".into(),
                right_value: "1 binary".into(),
                duration_s: 4.0,
                shot: None,
            },
            Scene::ProgressBar {
                title: Some("Coverage".into()),
                progress: 0.82,
                label: Some("scene types covered by brief syntax".into()),
                duration_s: 3.0,
                shot: None,
            },
            Scene::Callout {
                title: "Tip".into(),
                body: "Run `kf doctor project <dir>` before rendering.".into(),
                kind: "tip".into(),
                duration_s: 3.0,
                shot: None,
            },
            Scene::Callout {
                title: "Watch out".into(),
                body: "Em-dashes in quotes are 3 UTF-8 bytes — don't hardcode offsets.".into(),
                kind: "warning".into(),
                duration_s: 3.0,
                shot: None,
            },
            Scene::KpiGrid {
                title: "Release pulse".into(),
                duration_s: 5.0,
                shot: None,
                cells: vec![
                    KpiCell {
                        label: "PRs".into(),
                        value: "14".into(),
                        change: Some(18.0),
                        suffix: None,
                    },
                    KpiCell {
                        label: "Build".into(),
                        value: "11".into(),
                        change: Some(-22.0),
                        suffix: Some(" min".into()),
                    },
                    KpiCell {
                        label: "Bugs".into(),
                        value: "2".into(),
                        change: Some(-50.0),
                        suffix: None,
                    },
                    KpiCell {
                        label: "Demos".into(),
                        value: "5".into(),
                        change: None,
                        suffix: None,
                    },
                ],
            },
            Scene::BarChart {
                title: "Pipeline stage latency (ms)".into(),
                duration_s: 6.0,
                shot: None,
                bars: vec![
                    Bar {
                        label: "research".into(),
                        value: 0.95,
                        color: "#3aa0ff".into(),
                    },
                    Bar {
                        label: "proposal".into(),
                        value: 0.70,
                        color: "#ffcc00".into(),
                    },
                    Bar {
                        label: "script".into(),
                        value: 0.85,
                        color: "#6cd07a".into(),
                    },
                    Bar {
                        label: "compose".into(),
                        value: 0.40,
                        color: "#ff5a5a".into(),
                    },
                ],
            },
            Scene::CaptionOverlay {
                lines: vec![
                    "agentic".into(),
                    "type-safe".into(),
                    "FFmpeg-native".into(),
                    "no JS at render".into(),
                ],
                duration_s: 6.0,
                shot: None,
            },
            Scene::TextCard {
                title: "What's next".into(),
                body: "LineChart, PieChart, and per-stage human approval.".into(),
                duration_s: 4.0,
                shot: None,
            },
            Scene::TerminalScene {
                title: Some("build log".into()),
                prompt: "$ ".into(),
                accent_color: Some("#3aa0ff".into()),
                steps: vec![
                    TerminalStep::Cmd {
                        text: "kf render brief.md -o showcase.mp4".into(),
                        type_speed: 0.04,
                        hold_s: 0.2,
                    },
                    TerminalStep::Out {
                        text: "Plan: 14 scenes | total 47.8s".into(),
                        hold_s: 0.4,
                    },
                    TerminalStep::Out {
                        text: "Risk: 0.18 average (Strong)".into(),
                        hold_s: 0.4,
                    },
                    TerminalStep::Out {
                        text: "Filter graph: 28 chains | 0 rejects".into(),
                        hold_s: 0.4,
                    },
                    TerminalStep::Pill {
                        text: "READY".into(),
                        color: Some("#27c93f".into()),
                        hold_s: 2.0,
                    },
                ],
                duration_s: 6.0,
                shot: None,
            },
            Scene::EndTag {
                text: "kirkforge.video".into(),
                duration_s: 3.0,
                shot: None,
            },
        ],
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::compose::scene_kind_tag;

    #[test]
    fn showcase_demo_uses_all_twelve_scene_kinds() {
        // ponytail: the showcase demo is the regression net for the
        // whole DSL. Every scene type must appear at least once.
        let comp = showcase();
        let mut seen: Vec<&str> = comp.scenes.iter().map(scene_kind_tag).collect();
        seen.sort();
        seen.dedup();
        let want = vec![
            "bar_chart",
            "callout",
            "caption_overlay",
            "comparison",
            "end_tag",
            "hero_title",
            "kpi_grid",
            "progress_bar",
            "quote_card",
            "stat_card",
            "terminal_scene",
            "text_card",
        ];
        assert_eq!(
            seen,
            want,
            "showcase must use every scene kind; missing: {:?}",
            want.iter()
                .filter(|k| !seen.contains(k))
                .collect::<Vec<_>>()
        );
        // 45..=90s — long enough to be useful, short enough to render
        // in CI without timing out.
        let dur = comp.total_duration_s();
        assert!(
            (45.0..=90.0).contains(&dur),
            "showcase duration {dur}s should be in 45..=90s"
        );
    }
}
