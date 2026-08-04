//! Delivery promise classifier. Prevents silent downgrade from motion-led
//! to still-led. Mirrors `lib/delivery_promise.py` (8 enum variants).

use serde::{Deserialize, Serialize};
use strum::{Display, EnumIter};

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Display, EnumIter, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum PromiseType {
    MotionLed,
    SourceLed,
    DataExplainer,
    TeacherExplainer,
    ScreenDemo,
    AvatarPresenter,
    Hybrid,
    Localization,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct PromiseRules {
    pub still_fallback_allowed: bool,
    pub requires_video_generation: bool,
    pub min_motion_ratio: f32,
}

impl PromiseType {
    /// ponytail: explicit list rather than `EnumIter::iter()` so we control
    /// ordering for prompts ("consider these promises") without depending on
    /// `strum::IntoEnumIterator` at every callsite.
    pub fn all() -> &'static [PromiseType] {
        &[
            Self::MotionLed,
            Self::SourceLed,
            Self::DataExplainer,
            Self::TeacherExplainer,
            Self::ScreenDemo,
            Self::AvatarPresenter,
            Self::Hybrid,
            Self::Localization,
        ]
    }

    pub fn rules(self) -> PromiseRules {
        match self {
            Self::MotionLed => PromiseRules {
                still_fallback_allowed: false,
                requires_video_generation: true,
                min_motion_ratio: 0.7,
            },
            Self::AvatarPresenter => PromiseRules {
                still_fallback_allowed: false,
                requires_video_generation: true,
                min_motion_ratio: 0.3,
            },
            Self::SourceLed => PromiseRules {
                still_fallback_allowed: true,
                requires_video_generation: false,
                min_motion_ratio: 0.3,
            },
            Self::Hybrid => PromiseRules {
                still_fallback_allowed: true,
                requires_video_generation: false,
                min_motion_ratio: 0.2,
            },
            Self::DataExplainer
            | Self::TeacherExplainer
            | Self::ScreenDemo
            | Self::Localization => PromiseRules {
                still_fallback_allowed: true,
                requires_video_generation: false,
                min_motion_ratio: 0.0,
            },
        }
    }
}
