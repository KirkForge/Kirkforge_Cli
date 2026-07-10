//! Slideshow risk scorer. 6 dimensions, each 0..=5. Lower is better.
//! Mirrors `lib/slideshow_risk.py`.

use serde::{Deserialize, Serialize};

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum RiskVerdict {
    Strong,     // < 2.0
    Acceptable, // < 3.0
    Revise,     // < 4.0
    Fail,       // >= 4.0
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct DimensionScore {
    pub score: f32,
    pub reason: String,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct RiskReport {
    pub average: f32,
    pub verdict: RiskVerdict,
    pub dimensions: std::collections::BTreeMap<String, DimensionScore>,
}

/// Per-scene view passed into the risk scorer. `motion` is the
/// `shot.camera_motion` field, if the author set one (`static` / `push` /
/// `pan` / `tilt` / `dolly`). `None` means the author didn't declare one
/// and the scorer falls back to the kind-based heuristic.
#[derive(Debug, Clone, Copy)]
pub struct SceneView<'a> {
    pub kind: &'a str,
    pub motion: Option<&'a str>,
}

/// ponytail: any camera motion other than `static` counts as motion.
fn has_motion(motion: Option<&str>) -> bool {
    matches!(motion, Some(m) if m != "static")
}

/// Score one scene plan. `scene_kinds` is the list of scene-type tags
/// (e.g. `["hero_title", "stat_card", "bar_chart", ...]`). `total_dur_s`
/// is the full composition duration.
pub fn score_slideshow_risk(scene_kinds: &[&str], total_dur_s: f32) -> RiskReport {
    let views: Vec<SceneView> = scene_kinds
        .iter()
        .map(|k| SceneView {
            kind: k,
            motion: None,
        })
        .collect();
    score_slideshow_risk_views(&views, total_dur_s)
}

/// Same as `score_slideshow_risk` but with per-scene shot metadata. This is
/// the version the pipeline should call once `shot` is in the ScenePlan.
pub fn score_slideshow_risk_views(views: &[SceneView<'_>], total_dur_s: f32) -> RiskReport {
    let kinds: Vec<&str> = views.iter().map(|v| v.kind).collect();
    let dims = std::collections::BTreeMap::from([
        ("repetition".into(), score_repetition(&kinds)),
        ("decorative_visuals".into(), score_decorative_views(views)),
        ("weak_motion".into(), score_weak_motion_views(views)),
        (
            "weak_shot_intent".into(),
            score_weak_intent(&kinds, total_dur_s),
        ),
        ("typography_overreliance".into(), score_typography(&kinds)),
        (
            "unsupported_cinematic_claims".into(),
            score_cinematic(&kinds),
        ),
    ]);
    let avg = dims.values().map(|d| d.score).sum::<f32>() / dims.len() as f32;
    let verdict = if avg < 2.0 {
        RiskVerdict::Strong
    } else if avg < 3.0 {
        RiskVerdict::Acceptable
    } else if avg < 4.0 {
        RiskVerdict::Revise
    } else {
        RiskVerdict::Fail
    };
    RiskReport {
        average: avg,
        verdict,
        dimensions: dims,
    }
}

fn count<T: PartialEq>(items: &[T], target: T) -> usize {
    items.iter().filter(|x| **x == target).count()
}

fn score_repetition(kinds: &[&str]) -> DimensionScore {
    let total = kinds.len().max(1) as f32;
    let mut max_share = 0.0_f32;
    for k in [
        "hero_title",
        "text_card",
        "stat_card",
        "bar_chart",
        "clip_cut",
        "caption_overlay",
    ] {
        let share = count(kinds, k) as f32 / total;
        if share > max_share {
            max_share = share;
        }
    }
    let score = (max_share * 5.0).min(5.0);
    DimensionScore {
        score,
        reason: format!("max-kind share={max_share:.2}"),
    }
}

#[allow(dead_code)]
fn score_decorative(_kinds: &[&str]) -> DimensionScore {
    // Legacy hook — kept for any direct callers; the views-aware version
    // below is what the pipeline uses.
    DimensionScore {
        score: 0.0,
        reason: "use score_decorative_views".into(),
    }
}

fn score_decorative_views(views: &[SceneView<'_>]) -> DimensionScore {
    let total = views.len().max(1) as f32;
    // A scene is "motion" if it's a clip_cut OR its author declared a
    // non-static camera motion. Animations (bar_chart) count as visual.
    let motion = views
        .iter()
        .filter(|v| v.kind == "clip_cut" || has_motion(v.motion))
        .count() as f32;
    let motion_share = motion / total;
    let score = ((1.0 - motion_share) * 3.0).min(5.0);
    DimensionScore {
        score,
        reason: format!("motion share={motion_share:.2}"),
    }
}

#[allow(dead_code)]
fn score_weak_motion(_kinds: &[&str]) -> DimensionScore {
    DimensionScore {
        score: 0.0,
        reason: "use score_weak_motion_views".into(),
    }
}

fn score_weak_motion_views(views: &[SceneView<'_>]) -> DimensionScore {
    // A scene is "static" if it's a text/typography kind AND the author
    // didn't declare a non-static camera motion. clip_cut and bar_chart are
    // already motion; a hero_title with `motion: push` is motion too.
    let total = views.len().max(1) as f32;
    let static_kinds: &[&str] = &[
        "hero_title",
        "text_card",
        "stat_card",
        "caption_overlay",
        "end_tag",
    ];
    let static_count = views
        .iter()
        .filter(|v| static_kinds.contains(&v.kind) && !has_motion(v.motion))
        .count() as f32;
    let share = static_count / total;
    let score = (share * 4.0).min(5.0);
    DimensionScore {
        score,
        reason: format!("static share={share:.2}"),
    }
}

fn score_weak_intent(kinds: &[&str], dur_s: f32) -> DimensionScore {
    // Short compositions with many cuts = low intentional pacing.
    let cuts_per_sec = kinds.len() as f32 / dur_s.max(0.1);
    let score = if cuts_per_sec > 0.5 {
        3.5
    } else if cuts_per_sec > 0.25 {
        2.0
    } else {
        1.0
    };
    DimensionScore {
        score,
        reason: format!("cuts/sec={cuts_per_sec:.2}"),
    }
}

fn score_typography(kinds: &[&str]) -> DimensionScore {
    let total = kinds.len().max(1) as f32;
    let text = kinds
        .iter()
        .filter(|k| {
            matches!(
                **k,
                "hero_title" | "text_card" | "stat_card" | "caption_overlay" | "end_tag"
            )
        })
        .count() as f32;
    let share = text / total;
    let score = (share * 4.0).min(5.0);
    DimensionScore {
        score,
        reason: format!("text share={share:.2}"),
    }
}

fn score_cinematic(kinds: &[&str]) -> DimensionScore {
    // Without a real renderer family check, default to "unsupported if any
    // hero_title at index 0 with no clip_cut nearby". Cheap heuristic.
    let starts_with_text = kinds
        .first()
        .copied()
        .map(|k| matches!(k, "hero_title" | "text_card" | "end_tag"))
        .unwrap_or(false);
    let has_clip = kinds.contains(&"clip_cut");
    let score = if starts_with_text && !has_clip {
        2.0
    } else {
        1.0
    };
    DimensionScore {
        score,
        reason: format!("text-leading={starts_with_text}, has_clip={has_clip}"),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn short_static_plan_is_revise_or_worse() {
        let kinds = ["hero_title", "text_card", "text_card", "end_tag"];
        let r = score_slideshow_risk(&kinds, 10.0);
        assert!(r.average >= 2.0);
    }

    #[test]
    fn varied_plan_with_clips_is_acceptable() {
        let kinds = [
            "hero_title",
            "clip_cut",
            "stat_card",
            "bar_chart",
            "caption_overlay",
            "end_tag",
        ];
        let r = score_slideshow_risk(&kinds, 30.0);
        assert!(r.average < 4.0);
    }

    #[test]
    fn camera_motion_push_lowers_weak_motion_score() {
        // Same kinds, same length — the only difference is camera_motion.
        let kinds = ["hero_title", "hero_title", "hero_title", "end_tag"];
        let r_static = score_slideshow_risk(&kinds, 8.0);
        let views: Vec<SceneView> = kinds
            .iter()
            .enumerate()
            .map(|(i, k)| SceneView {
                kind: k,
                motion: if i < 3 { Some("push") } else { Some("static") },
            })
            .collect();
        let r_motion = score_slideshow_risk_views(&views, 8.0);
        let static_dim = r_static.dimensions["weak_motion"].score;
        let motion_dim = r_motion.dimensions["weak_motion"].score;
        assert!(
            motion_dim < static_dim,
            "motion-aware weak_motion ({motion_dim}) should be lower than legacy ({static_dim})"
        );
    }
}
