//! Whether a recording of a display can be sharp, judged from the display's modes.
//!
//! A recording has exactly the pixels macOS composes. A Retina panel in a normal (HiDPI) mode composes
//! two pixels per point, so recordings are crisp and zooms hold up. The same panel in a scaled
//! low-resolution mode ("2048 x 1280" on a 2560 x 1600 panel, say) composes one pixel per point: macOS
//! itself screenshots at that size, so there is nothing more to capture, and zooming into it looks soft.
//! This tells the two apart so the app can say so, instead of the video just looking poor.

use serde::Serialize;

/// One display mode: its size in points and in pixels (CoreGraphics reports both).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct ModeInfo {
    pub width: u64,
    pub pixel_width: u64,
}

impl ModeInfo {
    /// Pixels per point; 1.0 for a mode with no width (it cannot be judged).
    pub fn scale(&self) -> f64 {
        if self.width == 0 {
            1.0
        } else {
            self.pixel_width as f64 / self.width as f64
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum Sharpness {
    /// The current mode renders at 1.5 or more pixels per point: recordings are crisp.
    Sharp,
    /// A Retina-capable display running a 1x mode: recordings are as soft as the screen is.
    LowResolutionMode,
    /// A plain 1x display with no sharper mode to switch to: nothing to improve.
    Standard,
}

/// Judge `current` against every mode the display offers.
pub fn classify(current: ModeInfo, available: &[ModeInfo]) -> Sharpness {
    if current.scale() >= 1.5 {
        Sharpness::Sharp
    } else if available.iter().any(|m| m.scale() >= 2.0) {
        Sharpness::LowResolutionMode
    } else {
        Sharpness::Standard
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    const fn mode(width: u64, pixel_width: u64) -> ModeInfo {
        ModeInfo { width, pixel_width }
    }

    #[test]
    fn a_retina_panel_in_a_normal_mode_is_sharp() {
        // "looks like 1440 x 900" renders 2880 x 1800
        let all = [mode(1280, 2560), mode(1440, 2880), mode(2048, 2048)];
        assert_eq!(classify(mode(1440, 2880), &all), Sharpness::Sharp);
    }

    #[test]
    fn a_retina_panel_in_a_one_to_one_mode_is_flagged() {
        // this Mac: 2048 x 1280 pixels for 2048 x 1280 points, while the panel can do 2x modes
        let all = [mode(1280, 2560), mode(1440, 2880), mode(2048, 2048), mode(1680, 1680)];
        assert_eq!(classify(mode(2048, 2048), &all), Sharpness::LowResolutionMode);
    }

    #[test]
    fn an_ordinary_monitor_has_nothing_to_improve_and_is_not_nagged() {
        let all = [mode(1920, 1920), mode(1600, 1600), mode(1280, 1280)];
        assert_eq!(classify(mode(1920, 1920), &all), Sharpness::Standard);
    }

    #[test]
    fn a_fractional_scale_is_judged_by_pixels_per_point() {
        assert_eq!(classify(mode(2048, 3072), &[]), Sharpness::Sharp); // 1.5
        assert_eq!(classify(mode(2048, 2560), &[mode(1024, 2048)]), Sharpness::LowResolutionMode);
        // 1.25, can do 2x
    }

    #[test]
    fn a_mode_with_no_width_is_not_a_crash() {
        assert_eq!(mode(0, 0).scale(), 1.0);
        assert_eq!(classify(mode(0, 0), &[]), Sharpness::Standard);
    }

    #[test]
    fn it_serialises_as_a_plain_word_for_the_front_end() {
        assert_eq!(serde_json::to_string(&Sharpness::LowResolutionMode).unwrap(), "\"low_resolution_mode\"");
        assert_eq!(serde_json::to_string(&Sharpness::Sharp).unwrap(), "\"sharp\"");
    }
}
