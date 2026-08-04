//! Scene plan variation checker.
//!
//! ponytail: structural guard that runs BEFORE Assets. Catches the concrete
//! patterns that make a video feel like a slideshow — same shot size three
//! times in a row, 60%+ static movement, no hero moment, descriptions full
//! of "stunning"/"innovative" filler. Slideshow risk already exists; this
//! is the WRITING-side complement.
//!
//! Ported from OpenMontage's `lib/variation_checker.py`. Each violation
//! category adds ~0.6 to the score; verdict thresholds: strong < 2,
//! acceptable < 3, revise < 4, fail otherwise.

use std::collections::HashSet;

/// Camera movement tokens we consider "static" — drag-the-percent of these
/// across a scene plan is a structural risk.
const STATIC_MOVEMENTS: &[&str] = &["static", "unspecified"];

/// Generic phrases that signal lazy scene descriptions. Match
/// case-insensitively against each scene's description.
const GENERIC_PHRASES: &[&str] = &[
    "a person",
    "a beautiful",
    "modern",
    "futuristic",
    "cutting-edge",
    "in today's world",
    "sleek design",
    "innovative",
    "state-of-the-art",
    "next-generation",
    "revolutionary",
    "a professional",
    "dynamic",
    "vibrant",
    "stunning",
    "breathtaking",
    "amazing",
    "incredible",
    "powerful",
    "seamless",
    "elegant solution",
];

/// Lightweight view of a scene for variation scoring. Same idea as the
/// `SceneView` used by slideshow_risk — owned inputs to keep the checker
/// deserialisation-free.
#[derive(Debug, Clone)]
pub struct SceneView {
    /// Optional scene id for diagnostics.
    pub id: Option<String>,
    /// `wide` | `medium` | `close` | `insert` | `cutaway` | `unspecified`.
    pub shot_size: Option<String>,
    /// `static` | `push` | `pan` | `tilt` | `dolly` | `unspecified`.
    pub camera_movement: Option<String>,
    /// e.g. `high_key` | `low_key` | `natural` | None.
    pub lighting_key: Option<String>,
    /// Description / visual prompt for this scene.
    pub description: Option<String>,
    /// Whether this scene is flagged as the visual peak.
    pub hero_moment: bool,
    /// Texture / material keywords (e.g. `["rain-slicked", "neon"]`).
    pub texture_keywords: Option<Vec<String>>,
    /// Why this scene exists in the video.
    pub shot_intent: Option<String>,
}

#[derive(Debug, Clone, PartialEq)]
pub enum Verdict {
    Strong,
    Acceptable,
    Revise,
    Fail,
}

#[derive(Debug, Clone)]
pub struct VariationReport {
    pub score: f32,
    pub verdict: Verdict,
    pub violations: Vec<String>,
    pub suggestions: Vec<String>,
}

/// Score a scene plan for repetitive / generic structural patterns.
///
/// `scenes` slice accepts `&SceneView`; pass views you've built by projecting
/// from your pipeline-internal scene type. Empty input → fail with a single
/// "no scenes" violation (mirrors upstream behavior).
pub fn check_scene_variation(scenes: &[SceneView]) -> VariationReport {
    if scenes.is_empty() {
        return VariationReport {
            score: 5.0,
            verdict: Verdict::Fail,
            violations: vec!["No scenes to check".into()],
            suggestions: vec![],
        };
    }

    let mut violations: Vec<String> = vec![];
    let mut suggestions: Vec<String> = vec![];

    // --- Check 1: Shot size variety ---
    let sizes: Vec<&str> = scenes
        .iter()
        .map(|s| s.shot_size.as_deref().unwrap_or("unspecified"))
        .collect();
    if scenes.len() >= 4 {
        let mut counts: std::collections::HashMap<&str, usize> = std::collections::HashMap::new();
        for &sz in &sizes {
            *counts.entry(sz).or_insert(0) += 1;
        }
        if let Some((&most_common, most_common_count)) = counts.iter().max_by_key(|(_, c)| *c) {
            if *most_common_count as f32 / scenes.len() as f32 > 0.5 {
                violations.push(format!(
                    "Shot size '{most_common}' used in {most_common_count}/{} scenes ({:.0}%). \
                     Vary shot sizes for visual interest.",
                    scenes.len(),
                    *most_common_count as f32 / scenes.len() as f32 * 100.0,
                ));
                suggestions
                    .push("Mix wide establishing shots with close-ups for visual rhythm.".into());
            }
        }
    }

    // --- Check 2: Consecutive same-size shots ---
    let mut consecutive_same = 0;
    for i in 1..sizes.len() {
        if sizes[i] == sizes[i - 1] && sizes[i] != "unspecified" {
            consecutive_same += 1;
        }
    }
    if consecutive_same >= 3 {
        violations.push(format!(
            "{consecutive_same} consecutive same-size shots. \
             Vary shot sizes between scenes for editorial rhythm."
        ));
    }

    // --- Check 3: Static shot overuse ---
    let movements: Vec<&str> = scenes
        .iter()
        .map(|s| s.camera_movement.as_deref().unwrap_or("unspecified"))
        .collect();
    let static_count = movements
        .iter()
        .filter(|m| STATIC_MOVEMENTS.contains(m))
        .count();
    if scenes.len() >= 4 && static_count as f32 / scenes.len() as f32 > 0.6 {
        violations.push(format!(
            "{static_count}/{} scenes are static or unspecified movement. \
             Add intentional camera movement to at least 40% of scenes.",
            scenes.len(),
        ));
        suggestions.push(
            "Consider dolly_in for emphasis, tracking for energy, or crane for scale.".into(),
        );
    }

    // --- Check 4: Lighting variety ---
    let lightings: HashSet<String> = scenes
        .iter()
        .filter_map(|s| s.lighting_key.clone())
        .collect();
    if scenes.len() >= 4 && lightings.len() <= 1 {
        violations.push(format!(
            "Only {} unique lighting setup(s) across {} scenes. \
             Vary lighting to create mood shifts.",
            lightings.len(),
            scenes.len(),
        ));
    }

    // --- Check 5: Hero moment exists and is visually distinct ---
    let hero_indices: Vec<usize> = scenes
        .iter()
        .enumerate()
        .filter(|(_, s)| s.hero_moment)
        .map(|(i, _)| i)
        .collect();
    if scenes.len() >= 4 && hero_indices.is_empty() {
        violations.push(
            "No hero_moment flagged. Every video should have at least one visual peak.".into(),
        );
        suggestions.push("Mark the most impactful scene as hero_moment=true.".into());
    }
    for hi in &hero_indices {
        let hero_size = sizes[*hi];
        for offset in [-1i32, 1] {
            let n = (*hi as i32 + offset) as usize;
            if n < scenes.len() {
                let nsize = sizes[n];
                if !hero_size.is_empty()
                    && hero_size != "unspecified"
                    && hero_size == nsize
                    && nsize != "unspecified"
                {
                    let hero_id = scenes[*hi].id.clone().unwrap_or_else(|| format!("#{hi}"));
                    violations.push(format!(
                        "Hero scene '{hero_id}' has same shot size as neighbor. \
                         Hero moments should be visually distinct from surrounding scenes."
                    ));
                }
            }
        }
    }

    // --- Check 6: Description specificity ---
    let generic_count = scenes
        .iter()
        .filter(|s| {
            let d = s.description.as_deref().unwrap_or("").to_lowercase();
            GENERIC_PHRASES.iter().any(|p| d.contains(p))
        })
        .count();
    if generic_count as f32 >= scenes.len() as f32 * 0.3 {
        violations.push(format!(
            "{generic_count}/{} scenes use generic language. \
             Replace vague descriptions with specific visual details.",
            scenes.len(),
        ));
        suggestions.push(
            "Instead of 'a beautiful cityscape', try 'rain-slicked Tokyo intersection \
             at night, neon reflections in puddles, pedestrians with translucent umbrellas'."
                .into(),
        );
    }

    // --- Check 7: Texture keywords presence ---
    let textured = scenes
        .iter()
        .filter(|s| {
            s.texture_keywords
                .as_ref()
                .map(|v| !v.is_empty())
                .unwrap_or(false)
        })
        .count();
    if scenes.len() >= 4 && (textured as f32) < scenes.len() as f32 * 0.3 {
        violations.push(format!(
            "Only {textured}/{} scenes have texture_keywords. \
             Add texture descriptors to visual scenes for richer generation prompts.",
            scenes.len(),
        ));
    }

    // --- Check 8: Shot intent completeness ---
    let intented = scenes.iter().filter(|s| s.shot_intent.is_some()).count();
    if scenes.len() >= 4 && (intented as f32) < scenes.len() as f32 * 0.5 {
        violations.push(format!(
            "Only {intented}/{} scenes have shot_intent. \
             Every scene should explain WHY it exists in the video.",
            scenes.len(),
        ));
    }

    let score = (violations.len() as f32 * 0.6).min(5.0);
    let verdict = if score < 2.0 {
        Verdict::Strong
    } else if score < 3.0 {
        Verdict::Acceptable
    } else if score < 4.0 {
        Verdict::Revise
    } else {
        Verdict::Fail
    };

    VariationReport {
        score,
        verdict,
        violations,
        suggestions,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn v(size: Option<&str>, mv: Option<&str>) -> SceneView {
        SceneView {
            id: None,
            shot_size: size.map(String::from),
            camera_movement: mv.map(String::from),
            lighting_key: None,
            description: None,
            hero_moment: false,
            texture_keywords: None,
            shot_intent: None,
        }
    }

    #[test]
    fn empty_scenes_is_fail() {
        let r = check_scene_variation(&[]);
        assert_eq!(r.verdict, Verdict::Fail);
        assert_eq!(r.score, 5.0);
    }

    #[test]
    fn strong_plan_passes() {
        // 4 scenes, varied sizes, varied movements, every check passes.
        let scenes = vec![
            SceneView {
                id: Some("a".into()),
                shot_size: Some("wide".into()),
                camera_movement: Some("push".into()),
                lighting_key: Some("high_key".into()),
                description: Some("rain-slicked intersection, neon in puddles".into()),
                hero_moment: false,
                texture_keywords: Some(vec!["rain-slicked".into()]),
                shot_intent: Some("setup".into()),
            },
            SceneView {
                id: Some("b".into()),
                shot_size: Some("close".into()),
                camera_movement: Some("pan".into()),
                lighting_key: Some("low_key".into()),
                description: Some("translucent umbrellas drifting past".into()),
                hero_moment: false,
                texture_keywords: Some(vec!["translucent".into()]),
                shot_intent: Some("payoff".into()),
            },
            SceneView {
                id: Some("c".into()),
                shot_size: Some("insert".into()),
                camera_movement: Some("dolly".into()),
                lighting_key: Some("warm".into()),
                description: Some("a hand presses a brass button".into()),
                hero_moment: true,
                texture_keywords: Some(vec!["brass".into()]),
                shot_intent: Some("hero".into()),
            },
            SceneView {
                id: Some("d".into()),
                shot_size: Some("medium".into()),
                camera_movement: Some("tilt".into()),
                lighting_key: Some("natural".into()),
                description: Some("concrete alley behind a noodle shop".into()),
                hero_moment: false,
                texture_keywords: Some(vec!["concrete".into()]),
                shot_intent: Some("transition".into()),
            },
        ];
        let r = check_scene_variation(&scenes);
        assert_eq!(
            r.verdict,
            Verdict::Strong,
            "expected Strong got {:?} violations={:?}",
            r.verdict,
            r.violations
        );
        assert!(r.violations.is_empty());
    }

    #[test]
    fn repeated_shot_size_is_revise() {
        let scenes: Vec<_> = (0..5).map(|_i| v(Some("wide"), Some("push"))).collect();
        let r = check_scene_variation(&scenes);
        assert!(
            r.violations.iter().any(|v| v.contains("Shot size 'wide'")),
            "expected shot size variety violation: {:?}",
            r.violations
        );
        // 1 violation → 0.6 → strong. Need 4+ for revise.
    }

    #[test]
    fn static_overuse_is_revise() {
        let mut scenes = vec![v(Some("wide"), Some("push"))];
        for _ in 0..3 {
            scenes.push(v(Some("medium"), Some("static")));
        }
        scenes.push(v(Some("close"), Some("static")));
        let r = check_scene_variation(&scenes);
        assert!(
            r.violations.iter().any(|v| v.contains("static")),
            "expected static-overuse violation: {:?}",
            r.violations
        );
    }

    #[test]
    fn generic_phrases_flag() {
        let scenes = vec![
            SceneView {
                id: None,
                shot_size: Some("wide".into()),
                camera_movement: Some("push".into()),
                description: Some("a stunning modern cityscape with vibrant neon".into()),
                hero_moment: false,
                texture_keywords: Some(vec!["neon".into()]),
                shot_intent: Some("setup".into()),
                lighting_key: Some("natural".into()),
            },
            SceneView {
                id: None,
                shot_size: Some("close".into()),
                camera_movement: Some("pan".into()),
                description: Some("a beautiful futuristic sleek design".into()),
                hero_moment: true,
                texture_keywords: Some(vec!["metal".into()]),
                shot_intent: Some("hero".into()),
                lighting_key: Some("high_key".into()),
            },
            SceneView {
                id: None,
                shot_size: Some("medium".into()),
                camera_movement: Some("dolly".into()),
                description: Some("a professional state-of-the-art thing".into()),
                hero_moment: false,
                texture_keywords: Some(vec!["steel".into()]),
                shot_intent: Some("develop".into()),
                lighting_key: Some("low_key".into()),
            },
            SceneView {
                id: None,
                shot_size: Some("insert".into()),
                camera_movement: Some("tilt".into()),
                description: Some("an elegant solution that is seamless".into()),
                hero_moment: false,
                texture_keywords: Some(vec!["glass".into()]),
                shot_intent: Some("payoff".into()),
                lighting_key: Some("warm".into()),
            },
        ];
        let r = check_scene_variation(&scenes);
        // 4/4 = 100% generic phrases; violation expected.
        assert!(
            r.violations.iter().any(|v| v.contains("generic language")),
            "expected generic-phrase violation: {:?}",
            r.violations
        );
    }

    #[test]
    fn missing_hero_moment_is_violation() {
        let scenes: Vec<_> = (0..4)
            .map(|i| SceneView {
                id: Some(format!("s{i}")),
                shot_size: Some(if i % 2 == 0 { "wide" } else { "close" }.into()),
                camera_movement: Some("push".into()),
                lighting_key: Some("natural".into()),
                description: Some(format!("specific concrete description {i}")),
                hero_moment: false,
                texture_keywords: Some(vec!["specific".into()]),
                shot_intent: Some("step".into()),
            })
            .collect();
        let r = check_scene_variation(&scenes);
        assert!(
            r.violations.iter().any(|v| v.contains("hero_moment")),
            "expected hero_moment violation: {:?}",
            r.violations
        );
    }

    #[test]
    fn hero_with_neighbor_same_size_is_violation() {
        let scenes = vec![
            SceneView {
                id: Some("h_left".into()),
                shot_size: Some("close".into()),
                camera_movement: Some("dolly".into()),
                lighting_key: Some("warm".into()),
                description: Some("left neighbor close".into()),
                hero_moment: false,
                texture_keywords: Some(vec!["x".into()]),
                shot_intent: Some("setup".into()),
            },
            SceneView {
                id: Some("hero".into()),
                shot_size: Some("close".into()),
                camera_movement: Some("pan".into()),
                lighting_key: Some("warm".into()),
                description: Some("the hero shot".into()),
                hero_moment: true,
                texture_keywords: Some(vec!["x".into()]),
                shot_intent: Some("hero".into()),
            },
            SceneView {
                id: Some("h_right".into()),
                shot_size: Some("close".into()),
                camera_movement: Some("tilt".into()),
                lighting_key: Some("warm".into()),
                description: Some("right neighbor also close".into()),
                hero_moment: false,
                texture_keywords: Some(vec!["x".into()]),
                shot_intent: Some("payoff".into()),
            },
            SceneView {
                id: Some("tail".into()),
                shot_size: Some("wide".into()),
                camera_movement: Some("dolly".into()),
                lighting_key: Some("warm".into()),
                description: Some("end shot".into()),
                hero_moment: false,
                texture_keywords: Some(vec!["x".into()]),
                shot_intent: Some("payoff".into()),
            },
        ];
        let r = check_scene_variation(&scenes);
        assert!(
            r.violations
                .iter()
                .any(|v| v.contains("same shot size as neighbor")),
            "expected hero-neighbor violation: {:?}",
            r.violations
        );
    }
}
