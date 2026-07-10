//! Brief → scene-plan parser.
//!
//! Brief format (markdown-ish, blank lines ignored):
//!
//! ```text
//! Title here                       <- hero title
//! Optional subtitle                <- hero subtitle (if next line has no "-")
//! - 67% of mobile users            <- stat_card (number extracted)
//! - 3.2x productivity gain         <- stat_card
//! > Less is more. — Dieter Rams    <- quote_card (contains "—")
//! > Stay hungry. — Steve Jobs, Stanford
//! > kirkforge.video                <- end tag (lines starting with ">", no "—")
//! ```
//!
//! Numeric items also feed a synthesized bar chart (top 4 by magnitude).
//! Non-numeric items become caption overlays, grouped 3-per-scene.
//!
//! ponytail: regex only — no LLM. Upgrade by replacing `parse_brief` with
//! a model call returning the same `Brief` struct.

use regex::Regex;
use serde_json::json;

#[derive(Debug, Default, Clone)]
pub struct Stat {
    pub number: String,
    pub label: String,
    /// normalized 0..1 against the max magnitude in the brief, for chart bars
    pub value: f32,
}

#[derive(Debug, Default, Clone)]
pub struct Quote {
    pub text: String,
    pub author: Option<String>,
    pub source: Option<String>,
}

/// ponytail: `> X :: Y` and `> X :: Y :: Title` → Comparison scene.
/// The two values default to "Before"/"After" labels; explicit labels
/// aren't part of the brief syntax (callers who need them set them in
/// the scene plan by hand).
#[derive(Debug, Default, Clone)]
pub struct Comparison {
    pub left_value: String,
    pub right_value: String,
    pub title: Option<String>,
}

/// ponytail: `! Title :: Body :: kind` → Callout scene. `kind` is
/// optional and defaults to "tip" (matching `Scene::Callout`'s
/// default). Body and title are required; missing pieces make the
/// line fall through to plain caption handling.
#[derive(Debug, Default, Clone)]
pub struct CalloutBrief {
    pub title: String,
    pub body: String,
    pub kind: String,
}

#[derive(Debug, Default)]
pub struct Brief {
    pub title: String,
    pub subtitle: Option<String>,
    pub stats: Vec<Stat>,
    pub captions: Vec<String>,
    pub quotes: Vec<Quote>,
    pub comparisons: Vec<Comparison>,
    pub callouts: Vec<CalloutBrief>,
    pub end_tag: String,
}

pub fn parse_brief(text: &str) -> Brief {
    let lines: Vec<&str> = text
        .lines()
        .map(|l| l.trim())
        .filter(|l| !l.is_empty())
        .collect();

    let mut b = Brief {
        title: "Untitled".into(),
        ..Default::default()
    };

    let mut list_idx = 0; // points to first list-item line in `lines`
    if let Some(first) = lines.first() {
        b.title = strip_list_marker(first).to_string();
    }

    // Second line is subtitle iff it has no list marker.
    let mut cursor = 1;
    if let Some(second) = lines.get(1) {
        if !is_list_marker(second) && !second.starts_with('>') && !second.starts_with('!') {
            b.subtitle = Some(second.to_string());
            cursor = 2;
        }
    }

    let mut end_tag: Option<String> = None;
    for line in &lines[cursor..] {
        if let Some(s) = line.strip_prefix('!') {
            let body = s.trim();
            // ponytail: `!` prefix marks a callout. Three-part
            // `Title :: Body :: kind` is the full form; we also
            // accept two-part `Title :: Body` and default kind to
            // "tip".
            if let Some(cb) = parse_callout(body) {
                b.callouts.push(cb);
                continue;
            }
        }
        if let Some(s) = line.strip_prefix('>') {
            let body = s.trim();
            // ponytail: `::` splits a comparison. `>` lines with no
            // other marker go to the end tag (legacy rule preserved).
            if let Some(c) = parse_comparison(body) {
                b.comparisons.push(c);
                continue;
            }
            // ponytail: lines containing an em-dash are treated as
            // QuoteCards (author/source suffix). The legacy "last > wins
            // as end tag" rule still holds for plain `>` lines.
            if let Some(q) = parse_quote(body) {
                b.quotes.push(q);
            } else {
                end_tag = Some(body.to_string());
            }
            continue;
        }
        let item = match line.strip_prefix("- ") {
            Some(s) => s,
            None => continue,
        };
        if let Some(s) = extract_stat(item) {
            b.stats.push(s);
        } else {
            b.captions.push(item.to_string());
        }
        list_idx += 1;
    }
    b.end_tag = end_tag.unwrap_or_else(|| "kirkforge.video".into());

    // Normalize stats → 0..1 values for the optional chart. Use log scaling
    // so a "$4.2B" doesn't drown out "47%" — they're not commensurate.
    let mags: Vec<f32> = b
        .stats
        .iter()
        .filter_map(|s| parse_magnitude(&s.number))
        .filter(|m| *m > 0.0)
        .collect();
    if !mags.is_empty() {
        let min_log = mags.iter().cloned().fold(f32::INFINITY, f32::min).ln();
        let max_log = mags.iter().cloned().fold(f32::NEG_INFINITY, f32::max).ln();
        let span = (max_log - min_log).max(0.001);
        for s in &mut b.stats {
            if let Some(mag) = parse_magnitude(&s.number) {
                if mag > 0.0 {
                    let v = (mag.ln() - min_log) / span;
                    s.value = v.clamp(0.05, 1.0);
                }
            }
        }
    }

    // ignore list_idx (kept for future assertions); silence unused warning
    let _ = list_idx;
    b
}

fn is_list_marker(s: &str) -> bool {
    s.starts_with("- ") || s.starts_with("* ") || s.starts_with("• ")
}

fn strip_list_marker(s: &str) -> &str {
    if let Some(rest) = s.strip_prefix("- ") {
        return rest;
    }
    if let Some(rest) = s.strip_prefix("* ") {
        return rest;
    }
    if let Some(rest) = s.strip_prefix("• ") {
        return rest;
    }
    s
}

/// Extract a `CalloutBrief` from a `!`-line body if it splits on `::`.
/// Two-part (`! Title :: Body`) defaults kind to "tip". Three-part
/// (`! Title :: Body :: kind`) picks the kind. Missing pieces return
/// None so the line falls through.
fn parse_callout(body: &str) -> Option<CalloutBrief> {
    let parts: Vec<&str> = body.split("::").map(|p| p.trim()).collect();
    if parts.len() < 2 || parts.len() > 3 {
        return None;
    }
    let title = parts[0];
    let body_text = parts[1];
    if title.is_empty() || body_text.is_empty() {
        return None;
    }
    let kind = if parts.len() == 3 && !parts[2].is_empty() {
        parts[2].to_string()
    } else {
        "tip".into()
    };
    Some(CalloutBrief {
        title: title.to_string(),
        body: body_text.to_string(),
        kind,
    })
}

/// Extract a `Comparison` from a `>`-line body if it splits on `::`.
/// Two-part (`> X :: Y`) and three-part (`> X :: Y :: Title`) shapes
/// are both accepted. Anything with fewer than two non-empty parts
/// returns None so the line falls through to quote / end-tag parsing.
fn parse_comparison(body: &str) -> Option<Comparison> {
    let parts: Vec<&str> = body.split("::").map(|p| p.trim()).collect();
    if parts.len() < 2 || parts.len() > 3 {
        return None;
    }
    let left = parts[0];
    let right = parts[1];
    if left.is_empty() || right.is_empty() {
        return None;
    }
    let title = if parts.len() == 3 {
        let t = parts[2];
        if t.is_empty() {
            None
        } else {
            Some(t.to_string())
        }
    } else {
        None
    };
    Some(Comparison {
        left_value: left.to_string(),
        right_value: right.to_string(),
        title,
    })
}

/// Extract a `Quote` from a `>`-line body if it carries an em-dash
/// attribution. Returns None for plain end-tag lines (no `—`) so the
/// caller falls through to the end_tag branch.
fn parse_quote(body: &str) -> Option<Quote> {
    // ponytail: scan for the LAST " — " so a quote with an em-dash in
    // the body still parses (rare, but free). "First dash" gets
    // confused by typography in source text.
    let idx = body.rfind(" — ")?;
    let quote = body[..idx].trim().to_string();
    // ponytail: " — " is 5 bytes in UTF-8 (em-dash is 3 bytes). Don't
    // hardcode 3 here — slicing at idx+3 lands inside the em-dash and
    // panics. Use the search string's own byte length.
    let tail = body[idx + " — ".len()..].trim();
    if quote.is_empty() {
        return None;
    }
    // Tail can be "Author" or "Author, Source". Split on the FIRST comma
    // so commas inside the source text stay intact.
    let (author, source) = match tail.find(',') {
        Some(i) => (
            tail[..i].trim().to_string(),
            Some(tail[i + 1..].trim().to_string()),
        ),
        None => (tail.to_string(), None),
    };
    if author.is_empty() {
        return None;
    }
    Some(Quote {
        text: quote,
        author: Some(author),
        source,
    })
}

/// Extract a `Stat` from a list item if it contains a number.
fn extract_stat(item: &str) -> Option<Stat> {
    let re = Regex::new(r"(\$?\d+(?:\.\d+)?\s*[%bBmMkKxX]?)").unwrap();
    let m = re.find(item)?;
    let number = m.as_str().trim().to_string();
    // label = item minus the matched number, trimmed
    let label = format!(
        "{}{}",
        item[..m.start()].trim_end(),
        if m.end() < item.len() {
            format!(" {}", item[m.end()..].trim_start())
        } else {
            String::new()
        },
    );
    let label = label.trim().to_string();
    let value = parse_magnitude(&number).unwrap_or(0.0);
    Some(Stat {
        number,
        label,
        value,
    })
}

fn parse_magnitude(s: &str) -> Option<f32> {
    let s = s.trim().replace(['$', ',', ' '], "");
    if s.is_empty() {
        return None;
    }
    let (num_part, suffix) = match s.find(|c: char| !c.is_ascii_digit() && c != '.') {
        Some(i) => (&s[..i], &s[i..]),
        None => (s.as_str(), ""),
    };
    let n: f32 = num_part.parse().ok()?;
    let mult: f32 = match suffix.trim() {
        "k" | "K" => 1_000.0,
        "m" | "M" => 1_000_000.0,
        "b" | "B" => 1_000_000_000.0,
        "x" | "X" => 1.0,
        "%" => n.max(1.0),
        _ => 1.0,
    };
    Some(n * mult)
}

/// Synthesize the scene-plan JSON consumed by Compose.
pub fn scene_plan_from_brief(b: &Brief) -> serde_json::Value {
    let mut scenes: Vec<serde_json::Value> = vec![json!({
        "type": "hero_title",
        "title": b.title,
        "subtitle": b.subtitle,
        "duration_s": 3.0,
    })];

    for s in &b.stats {
        scenes.push(json!({
            "type": "stat_card",
            "number": s.number,
            "label": s.label,
            "duration_s": 3.0,
        }));
    }

    if b.stats.len() >= 3 {
        let palette = ["#3aa0ff", "#ffcc00", "#6cd07a", "#ff5a5a", "#bb86fc"];
        let bars: Vec<_> = b
            .stats
            .iter()
            .take(5)
            .enumerate()
            .map(|(i, s)| {
                json!({
                    "label": s.label.chars().take(18).collect::<String>(),
                    "value": s.value,
                    "color": palette[i % palette.len()],
                })
            })
            .collect();
        let title = format!("{} by magnitude", b.title);
        scenes.push(json!({
            "type": "bar_chart",
            "title": title,
            "bars": bars,
            "duration_s": 6.0,
        }));
    }

    for chunk in b.captions.chunks(3) {
        scenes.push(json!({
            "type": "caption_overlay",
            "lines": chunk,
            "duration_s": 4.0,
        }));
    }

    // ponytail: quote cards sit between captions and end tag so the
    // visual rhythm goes hero → stats → chart → captions → quote(s) →
    // close. Quotes are 4.0s each (a touch longer than stat cards
    // because reading time matters more for prose).
    for q in &b.quotes {
        scenes.push(json!({
            "type": "quote_card",
            "quote": q.text,
            "author": q.author,
            "source": q.source,
            "duration_s": 4.0,
        }));
    }

    // ponytail: comparisons follow quotes. 3.5s each — shorter than
    // quotes (less prose to read) but longer than stat cards (two
    // values to compare).
    for c in &b.comparisons {
        scenes.push(json!({
            "type": "comparison",
            "title": c.title,
            "left_label": "Before",
            "left_value": c.left_value,
            "right_label": "After",
            "right_value": c.right_value,
            "duration_s": 3.5,
        }));
    }

    // ponytail: callouts sit alongside comparisons (similar pacing —
    // they read quickly). 3.0s each.
    for c in &b.callouts {
        scenes.push(json!({
            "type": "callout",
            "title": c.title,
            "body": c.body,
            "kind": c.kind,
            "duration_s": 3.0,
        }));
    }

    scenes.push(json!({
        "type": "end_tag",
        "title": b.end_tag,
        "duration_s": 2.0,
    }));

    json!({ "kind": "scene_plan", "scenes": scenes })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parses_title_subtitle_stats_endtag() {
        let b = parse_brief("FocusFlow\nDistraction-free deep work\n- 3.2x output per hour\n- -47% context switches\n> focusflow.app\n");
        assert_eq!(b.title, "FocusFlow");
        assert_eq!(b.subtitle.as_deref(), Some("Distraction-free deep work"));
        assert_eq!(b.stats.len(), 2);
        assert_eq!(b.stats[0].number, "3.2x");
        assert_eq!(b.end_tag, "focusflow.app");
    }

    #[test]
    fn extract_stat_handles_percent_and_currency() {
        let s = extract_stat("67% of mobile users").unwrap();
        assert_eq!(s.number, "67%");
        assert!(s.value > 0.0);
        let s = extract_stat("$4.2B market").unwrap();
        assert_eq!(s.number, "$4.2B");
        assert!(s.value > 0.0);
    }

    #[test]
    fn normalization_handles_mixed_units() {
        let b = parse_brief("Hi\n- 47%\n- $4.2B\n- 3.2x\n");
        // smallest → 0.05, largest → 1.0, middle → somewhere in (0.05, 1.0)
        let vals: Vec<f32> = b.stats.iter().map(|s| s.value).collect();
        assert!(vals.iter().all(|v| *v >= 0.05 && *v <= 1.0));
        let max = vals.iter().cloned().fold(0.0_f32, f32::max);
        let min = vals.iter().cloned().fold(1.0_f32, f32::min);
        assert!((max - 1.0).abs() < 0.01);
        assert!((min - 0.05).abs() < 0.01);
    }

    #[test]
    fn non_numeric_items_become_captions() {
        let b = parse_brief("Hi\n- 50% claim\n- agentic\n- FFmpeg-native\n- Rust\n");
        assert_eq!(b.stats.len(), 1);
        assert_eq!(b.captions, vec!["agentic", "FFmpeg-native", "Rust"]);
    }

    #[test]
    fn quote_line_with_em_dash_becomes_quote_card() {
        let b = parse_brief("Hi\n> Less is more. — Dieter Rams\n> focusflow.app\n");
        assert_eq!(b.quotes.len(), 1);
        assert_eq!(b.quotes[0].text, "Less is more.");
        assert_eq!(b.quotes[0].author.as_deref(), Some("Dieter Rams"));
        assert_eq!(b.quotes[0].source, None);
        // The trailing `> focusflow.app` still becomes the end tag — the
        // legacy "last `>` wins" rule is preserved when the line has no
        // em-dash.
        assert_eq!(b.end_tag, "focusflow.app");
    }

    #[test]
    fn quote_line_with_author_and_source_splits_on_first_comma() {
        let b = parse_brief("Hi\n> Stay hungry, stay foolish. — Steve Jobs, Stanford 2005\n");
        assert_eq!(b.quotes.len(), 1);
        assert_eq!(b.quotes[0].author.as_deref(), Some("Steve Jobs"));
        assert_eq!(b.quotes[0].source.as_deref(), Some("Stanford 2005"));
        // No plain `>` line → default end tag.
        assert_eq!(b.end_tag, "kirkforge.video");
    }

    #[test]
    fn multiple_quotes_preserve_order_in_scene_plan() {
        let b = parse_brief(
            "Hi\n\
             > First quote. — Author A\n\
             - 50% stat\n\
             > Second quote. — Author B, Source B\n\
             > kirkforge.video\n",
        );
        assert_eq!(b.quotes.len(), 2);
        let plan = scene_plan_from_brief(&b);
        let kinds: Vec<&str> = plan["scenes"]
            .as_array()
            .unwrap()
            .iter()
            .map(|s| s["type"].as_str().unwrap())
            .collect();
        // hero_title, stat_card, quote_card, quote_card, end_tag.
        // scene_plan_from_brief emits stats first, then quotes, regardless of
        // the order they appeared in the source brief.
        assert_eq!(
            kinds,
            vec![
                "hero_title",
                "stat_card",
                "quote_card",
                "quote_card",
                "end_tag"
            ]
        );
    }

    #[test]
    fn plain_greater_than_line_still_becomes_end_tag_no_quotes() {
        let b = parse_brief("Hi\n> kirkforge.video\n");
        assert_eq!(b.quotes.len(), 0);
        assert_eq!(b.end_tag, "kirkforge.video");
    }

    #[test]
    fn comparison_two_part_parses_left_and_right_values() {
        let b = parse_brief("Hi\n> 32s :: 12s\n> kirkforge.video\n");
        assert_eq!(b.comparisons.len(), 1);
        assert_eq!(b.comparisons[0].left_value, "32s");
        assert_eq!(b.comparisons[0].right_value, "12s");
        assert_eq!(b.comparisons[0].title, None);
        // The plain `>` line still won as end tag — comparisons don't
        // suppress that legacy rule.
        assert_eq!(b.end_tag, "kirkforge.video");
    }

    #[test]
    fn comparison_three_part_picks_up_title() {
        let b = parse_brief("Hi\n> 60s :: 8s :: Build time\n");
        assert_eq!(b.comparisons.len(), 1);
        assert_eq!(b.comparisons[0].left_value, "60s");
        assert_eq!(b.comparisons[0].right_value, "8s");
        assert_eq!(b.comparisons[0].title.as_deref(), Some("Build time"));
    }

    #[test]
    fn comparison_with_quote_and_endtag_in_same_brief_preserves_all_three() {
        // ponytail: the new `::` parser must not eat em-dash quotes
        // or plain end-tag lines. All three forms coexist.
        let b = parse_brief(
            "Hi\n\
                              > 32s :: 12s\n\
                              > Less is more. — Dieter Rams\n\
                              > kirkforge.video\n",
        );
        assert_eq!(b.comparisons.len(), 1);
        assert_eq!(b.comparisons[0].left_value, "32s");
        assert_eq!(b.quotes.len(), 1);
        assert_eq!(b.quotes[0].author.as_deref(), Some("Dieter Rams"));
        assert_eq!(b.end_tag, "kirkforge.video");
    }

    #[test]
    fn comparison_in_scene_plan_emits_comparison_type_after_quotes() {
        let b = parse_brief(
            "Hi\n\
                              > Less is more. — Author\n\
                              > 32s :: 12s\n\
                              > kirkforge.video\n",
        );
        let plan = scene_plan_from_brief(&b);
        let kinds: Vec<&str> = plan["scenes"]
            .as_array()
            .unwrap()
            .iter()
            .map(|s| s["type"].as_str().unwrap())
            .collect();
        // hero_title, quote_card, comparison, end_tag
        assert_eq!(
            kinds,
            vec!["hero_title", "quote_card", "comparison", "end_tag"]
        );
        // The comparison entry has default labels.
        let cmp = &plan["scenes"][2];
        assert_eq!(cmp["left_label"], "Before");
        assert_eq!(cmp["right_label"], "After");
    }

    #[test]
    fn callout_three_part_parses_title_body_and_kind() {
        let b = parse_brief(
            "Hi\n\
                              ! Watch out :: Don't skip the dry-run :: warning\n\
                              > kirkforge.video\n",
        );
        assert_eq!(b.callouts.len(), 1);
        assert_eq!(b.callouts[0].title, "Watch out");
        assert_eq!(b.callouts[0].body, "Don't skip the dry-run");
        assert_eq!(b.callouts[0].kind, "warning");
    }

    #[test]
    fn callout_two_part_defaults_kind_to_tip() {
        let b = parse_brief("Hi\n! Tip :: Start small.\n");
        assert_eq!(b.callouts.len(), 1);
        assert_eq!(b.callouts[0].title, "Tip");
        assert_eq!(b.callouts[0].body, "Start small.");
        assert_eq!(b.callouts[0].kind, "tip");
    }

    #[test]
    fn callout_in_scene_plan_emits_callout_type_after_comparisons() {
        let b = parse_brief(
            "Hi\n\
                              > 32s :: 12s\n\
                              ! Tip :: Ship it :: tip\n\
                              > kirkforge.video\n",
        );
        let plan = scene_plan_from_brief(&b);
        let kinds: Vec<&str> = plan["scenes"]
            .as_array()
            .unwrap()
            .iter()
            .map(|s| s["type"].as_str().unwrap())
            .collect();
        // hero_title, comparison, callout, end_tag
        assert_eq!(
            kinds,
            vec!["hero_title", "comparison", "callout", "end_tag"]
        );
        let callout = &plan["scenes"][2];
        assert_eq!(callout["title"], "Tip");
        assert_eq!(callout["body"], "Ship it");
        assert_eq!(callout["kind"], "tip");
    }
}
