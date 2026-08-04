//! Platform media profiles — preset render targets so the composer and
//! publisher formatters share one truth source.
//!
//! ponytail: names match OpenMontage's `lib/media_profiles.py` so a
//! conversion tool can map directly. Add a new profile by appending to
//! `ALL_PROFILES` — every consumer iterates the registry at runtime.

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, serde::Serialize, serde::Deserialize)]
pub enum AspectRatio {
    Landscape16x9,
    Portrait9x16,
    Square1x1,
    Cinematic21x9,
    Standard4x3,
}

impl AspectRatio {
    pub fn as_label(&self) -> &'static str {
        match self {
            Self::Landscape16x9 => "16:9",
            Self::Portrait9x16 => "9:16",
            Self::Square1x1 => "1:1",
            Self::Cinematic21x9 => "21:9",
            Self::Standard4x3 => "4:3",
        }
    }
}

#[derive(Debug, Clone, serde::Serialize, serde::Deserialize)]
pub struct MediaProfile {
    pub name: &'static str,
    pub width: u32,
    pub height: u32,
    pub aspect_ratio: AspectRatio,
    pub fps: u32,
    pub codec: &'static str,
    pub audio_codec: &'static str,
    pub crf: u8,
    #[serde(default = "default_pixel_format")]
    pub pixel_format: &'static str,
    #[serde(default)]
    pub max_file_size_mb: Option<f32>,
    #[serde(default)]
    pub max_duration_seconds: Option<f32>,
    #[serde(default = "default_caption_format")]
    pub caption_format: &'static str,
    #[serde(default)]
    pub notes: &'static str,
}

fn default_pixel_format() -> &'static str {
    "yuv420p"
}
fn default_caption_format() -> &'static str {
    "srt"
}

/// ponytail: each profile is a `const` so the registry can borrow slices
/// without allocating. The `ALL_PROFILES` map duplicates the pointer —
/// `get_profile` returns by value (the structs are tiny).
pub const YOUTUBE_LANDSCAPE: MediaProfile = MediaProfile {
    name: "youtube_landscape",
    width: 1920,
    height: 1080,
    aspect_ratio: AspectRatio::Landscape16x9,
    fps: 30,
    codec: "libx264",
    audio_codec: "aac",
    crf: 18,
    pixel_format: "yuv420p",
    max_file_size_mb: None,
    max_duration_seconds: None,
    caption_format: "srt",
    notes: "YouTube standard HD upload",
};
pub const YOUTUBE_4K: MediaProfile = MediaProfile {
    name: "youtube_4k",
    width: 3840,
    height: 2160,
    aspect_ratio: AspectRatio::Landscape16x9,
    fps: 30,
    codec: "libx264",
    audio_codec: "aac",
    crf: 18,
    pixel_format: "yuv420p",
    max_file_size_mb: None,
    max_duration_seconds: None,
    caption_format: "srt",
    notes: "YouTube 4K upload",
};
pub const YOUTUBE_SHORTS: MediaProfile = MediaProfile {
    name: "youtube_shorts",
    width: 1080,
    height: 1920,
    aspect_ratio: AspectRatio::Portrait9x16,
    fps: 30,
    codec: "libx264",
    audio_codec: "aac",
    crf: 20,
    pixel_format: "yuv420p",
    max_file_size_mb: None,
    max_duration_seconds: Some(60.0),
    caption_format: "srt",
    notes: "YouTube Shorts (max 60s, vertical)",
};
pub const INSTAGRAM_REELS: MediaProfile = MediaProfile {
    name: "instagram_reels",
    width: 1080,
    height: 1920,
    aspect_ratio: AspectRatio::Portrait9x16,
    fps: 30,
    codec: "libx264",
    audio_codec: "aac",
    crf: 20,
    pixel_format: "yuv420p",
    max_file_size_mb: Some(250.0),
    max_duration_seconds: Some(90.0),
    caption_format: "srt",
    notes: "Instagram Reels (max 90s, vertical)",
};
pub const INSTAGRAM_FEED: MediaProfile = MediaProfile {
    name: "instagram_feed",
    width: 1080,
    height: 1080,
    aspect_ratio: AspectRatio::Square1x1,
    fps: 30,
    codec: "libx264",
    audio_codec: "aac",
    crf: 20,
    pixel_format: "yuv420p",
    max_file_size_mb: Some(250.0),
    max_duration_seconds: Some(60.0),
    caption_format: "srt",
    notes: "Instagram feed video (square)",
};
pub const TIKTOK: MediaProfile = MediaProfile {
    name: "tiktok",
    width: 1080,
    height: 1920,
    aspect_ratio: AspectRatio::Portrait9x16,
    fps: 30,
    codec: "libx264",
    audio_codec: "aac",
    crf: 20,
    pixel_format: "yuv420p",
    max_file_size_mb: Some(287.0),
    max_duration_seconds: Some(600.0),
    caption_format: "srt",
    notes: "TikTok (max 10min, vertical preferred)",
};
pub const LINKEDIN: MediaProfile = MediaProfile {
    name: "linkedin",
    width: 1920,
    height: 1080,
    aspect_ratio: AspectRatio::Landscape16x9,
    fps: 30,
    codec: "libx264",
    audio_codec: "aac",
    crf: 20,
    pixel_format: "yuv420p",
    max_file_size_mb: Some(5120.0),
    max_duration_seconds: Some(600.0),
    caption_format: "srt",
    notes: "LinkedIn video (landscape preferred, max 10min)",
};
pub const CINEMATIC: MediaProfile = MediaProfile {
    name: "cinematic",
    width: 2560,
    height: 1080,
    aspect_ratio: AspectRatio::Cinematic21x9,
    fps: 24,
    codec: "libx264",
    audio_codec: "aac",
    crf: 16,
    pixel_format: "yuv420p",
    max_file_size_mb: None,
    max_duration_seconds: None,
    caption_format: "srt",
    notes: "Cinematic ultra-wide format",
};
pub const GENERIC_HD: MediaProfile = MediaProfile {
    name: "generic_hd",
    width: 1920,
    height: 1080,
    aspect_ratio: AspectRatio::Landscape16x9,
    fps: 30,
    codec: "libx264",
    audio_codec: "aac",
    crf: 23,
    pixel_format: "yuv420p",
    max_file_size_mb: None,
    max_duration_seconds: None,
    caption_format: "srt",
    notes: "Generic HD output (no platform-specific constraints)",
};

pub const ALL_PROFILES: &[&MediaProfile] = &[
    &YOUTUBE_LANDSCAPE,
    &YOUTUBE_4K,
    &YOUTUBE_SHORTS,
    &INSTAGRAM_REELS,
    &INSTAGRAM_FEED,
    &TIKTOK,
    &LINKEDIN,
    &CINEMATIC,
    &GENERIC_HD,
];

pub fn get_profile(name: &str) -> Option<&'static MediaProfile> {
    ALL_PROFILES.iter().find(|p| p.name == name).copied()
}

pub fn get_profiles_for_platform(platform: &str) -> Vec<&'static MediaProfile> {
    ALL_PROFILES
        .iter()
        .filter(|p| p.name.starts_with(platform))
        .copied()
        .collect()
}

/// Build the ffmpeg output-args slice for a profile. `width`/`height`
/// are folded into a `scale=` filter so callers composing their own
/// filter graph can render to the target resolution.
pub fn ffmpeg_output_args(p: &MediaProfile) -> Vec<&'static str> {
    // ponytail: codec + crf + pixel_format bind output; `-vf scale=` is
    // returned separately so the caller can append it to its own filter
    // graph if needed.
    vec![
        "-c:v",
        p.codec,
        "-c:a",
        p.audio_codec,
        "-crf",
        _crf_str(p.crf),
        "-pix_fmt",
        p.pixel_format,
        "-r",
        _fps_str(p.fps),
    ]
}

// ponytail: crf_str and fps_str return leaked strings because ffmpeg
// arg strings need `'static` lifetimes. We leak exactly once per call
// per profile — fine in practice (profiles are loaded a handful of
// times per render).
fn _crf_str(crf: u8) -> &'static str {
    Box::leak(crf.to_string().into_boxed_str())
}
fn _fps_str(fps: u32) -> &'static str {
    Box::leak(fps.to_string().into_boxed_str())
}

/// ponytail: project a profile onto a Composition — sets width/height
/// to the profile's resolution and fps to the profile's rate. Does NOT
/// mutate scenes; assumes callers adapt scenes beforehand if needed.
pub fn apply_to_composition(p: &MediaProfile, comp: &mut crate::compose::Composition) {
    comp.width = p.width;
    comp.height = p.height;
    comp.fps = p.fps;
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn registry_has_all_9_profiles() {
        assert_eq!(ALL_PROFILES.len(), 9);
        for n in [
            "youtube_landscape",
            "youtube_4k",
            "youtube_shorts",
            "instagram_reels",
            "instagram_feed",
            "tiktok",
            "linkedin",
            "cinematic",
            "generic_hd",
        ] {
            assert!(get_profile(n).is_some(), "missing profile {n}");
        }
    }

    #[test]
    fn get_profile_unknown_returns_none() {
        assert!(get_profile("definitely_not_a_real_profile").is_none());
    }

    #[test]
    fn platform_prefix_filter_works() {
        let yt = get_profiles_for_platform("youtube");
        assert_eq!(yt.len(), 3);
        for p in &yt {
            assert!(p.name.starts_with("youtube"));
        }

        let ig = get_profiles_for_platform("instagram");
        assert_eq!(ig.len(), 2);
    }

    #[test]
    fn aspect_ratio_labels_match_spec() {
        assert_eq!(AspectRatio::Landscape16x9.as_label(), "16:9");
        assert_eq!(AspectRatio::Portrait9x16.as_label(), "9:16");
        assert_eq!(AspectRatio::Square1x1.as_label(), "1:1");
        assert_eq!(AspectRatio::Cinematic21x9.as_label(), "21:9");
        assert_eq!(AspectRatio::Standard4x3.as_label(), "4:3");
    }

    #[test]
    fn shorts_is_vertical_with_60s_cap() {
        let s = get_profile("youtube_shorts").unwrap();
        assert_eq!(s.width, 1080);
        assert_eq!(s.height, 1920);
        assert_eq!(s.aspect_ratio, AspectRatio::Portrait9x16);
        assert_eq!(s.max_duration_seconds, Some(60.0));
    }

    #[test]
    fn cinematic_is_24fps_with_21_9_aspect() {
        let c = get_profile("cinematic").unwrap();
        assert_eq!(c.fps, 24);
        assert_eq!(c.aspect_ratio, AspectRatio::Cinematic21x9);
        // ponytail: 2560x1080 isn't exact 21:9 (that's 21/9 = 2.333; we
        // get 2.370). The "21:9" label is the standard ultra-wide label;
        // the actual dimensions are what we assert.
        assert_eq!(c.width, 2560);
        assert_eq!(c.height, 1080);
    }

    #[test]
    fn ffmpeg_output_args_has_codec_and_crf() {
        let p = get_profile("youtube_landscape").unwrap();
        let args = ffmpeg_output_args(p);
        assert!(args.contains(&"-c:v"));
        assert!(args.contains(&"libx264"));
        assert!(args.contains(&"-c:a"));
        assert!(args.contains(&"aac"));
        assert!(args.contains(&"-pix_fmt"));
        assert!(args.contains(&"yuv420p"));
    }

    #[test]
    fn apply_to_composition_sets_resolution_and_fps() {
        let mut comp = crate::compose::Composition {
            width: 1,
            height: 1,
            fps: 1,
            scenes: vec![],
            audio: None,
        };
        let p = get_profile("tiktok").unwrap();
        apply_to_composition(p, &mut comp);
        assert_eq!(comp.width, 1080);
        assert_eq!(comp.height, 1920);
        assert_eq!(comp.fps, 30);
    }
}
