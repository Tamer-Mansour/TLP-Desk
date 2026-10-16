//! Quality presets: what a viewer's `SetQuality` resolves to on this host.

use tlp_desk_proto::{QualityPreset, QualitySettings};

/// Speed preset: frames capped at 720p.
pub(crate) const SPEED_CAP: (u32, u32) = (1280, 720);
/// Quality preset: native resolution up to the H.264 encoder's limit.
pub(crate) const QUALITY_CAP: (u32, u32) = (3840, 2160);
/// Smallest size cap a viewer may ask for (anything smaller is unreadable).
const MIN_CAP: (u32, u32) = (320, 180);
/// Largest size cap a viewer may ask for (8K; the encoder halves anything above its limit).
const MAX_CAP: (u32, u32) = (7680, 4320);
/// Frame-rate range a viewer may ask for.
const FPS_RANGE: (u32, u32) = (1, 120);
/// Fixed-bitrate range a viewer may ask for, kbps.
const FIXED_KBPS_RANGE: (u32, u32) = (100, 200_000);

/// How the encoder bitrate is chosen.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum BitrateMode {
    /// The adaptive controller picks the bitrate; its ceiling is this percentage of the
    /// default bitrate for the frame size and rate (`EncoderConfig::for_screen`).
    Adaptive {
        /// Ceiling in percent of the default bitrate.
        ceiling_pct: u32,
    },
    /// The viewer asked for a fixed bitrate (kbps); no adaptation.
    Fixed(u32),
}

/// A validated capture/encode target derived from a viewer's [`QualitySettings`].
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct QualityTarget {
    /// The preset this came from.
    pub preset: QualityPreset,
    /// Frames are scaled down (on the GPU) to fit this size; oriented like the monitor.
    pub max_size: (u32, u32),
    /// Target frame rate.
    pub fps: u32,
    /// Bitrate policy.
    pub bitrate: BitrateMode,
}

impl QualityTarget {
    /// The host's own default (what applies before any `SetQuality`): the `Balanced` preset
    /// with the host's configured cap and frame rate.
    pub fn host_default(cap: (u32, u32), fps: u32) -> Self {
        Self {
            preset: QualityPreset::Balanced,
            max_size: clamp_cap(cap),
            fps: fps.clamp(FPS_RANGE.0, FPS_RANGE.1),
            bitrate: BitrateMode::Adaptive { ceiling_pct: 100 },
        }
    }

    /// Resolves a viewer request (hostile input: every field is clamped).
    ///
    /// * `Speed`: 1280x720 cap, host fps, adaptive with a low ceiling (60 %).
    /// * `Balanced`: the host default (1920x1200 unless configured), adaptive at 100 %.
    /// * `Quality`: native up to 3840x2160, adaptive with a high ceiling (160 %).
    /// * `Custom`: the explicit fields; zero size/fps = host default, `None` bitrate = adaptive.
    ///
    /// Presets ignore the explicit fields so the viewer menu cannot produce surprising mixes.
    pub fn resolve(req: &QualitySettings, default: QualityTarget) -> Self {
        let adaptive = |ceiling_pct| BitrateMode::Adaptive { ceiling_pct };
        match req.preset {
            QualityPreset::Speed => Self {
                preset: QualityPreset::Speed,
                max_size: SPEED_CAP,
                fps: default.fps,
                bitrate: adaptive(60),
            },
            QualityPreset::Balanced => Self {
                preset: QualityPreset::Balanced,
                ..default
            },
            QualityPreset::Quality => Self {
                preset: QualityPreset::Quality,
                max_size: QUALITY_CAP,
                fps: default.fps,
                bitrate: adaptive(160),
            },
            QualityPreset::Custom => {
                let max_size = if req.max_width == 0 || req.max_height == 0 {
                    default.max_size
                } else {
                    clamp_cap((req.max_width, req.max_height))
                };
                let fps = if req.fps == 0 {
                    default.fps
                } else {
                    req.fps.clamp(FPS_RANGE.0, FPS_RANGE.1)
                };
                let bitrate = match req.bitrate_kbps {
                    Some(k) => BitrateMode::Fixed(k.clamp(FIXED_KBPS_RANGE.0, FIXED_KBPS_RANGE.1)),
                    None => adaptive(100),
                };
                Self {
                    preset: QualityPreset::Custom,
                    max_size,
                    fps,
                    bitrate,
                }
            }
        }
    }
}

fn clamp_cap((w, h): (u32, u32)) -> (u32, u32) {
    (w.clamp(MIN_CAP.0, MAX_CAP.0), h.clamp(MIN_CAP.1, MAX_CAP.1))
}

/// The size cap to open a capturer with for a `monitor` of the given size: `cap` turned to the
/// monitor's orientation (a portrait monitor gets a portrait cap, so it keeps its detail), then
/// scaled by `scale_permille` (the adaptive controller's resolution step, 1000 = full).
pub fn capture_cap(monitor: (u32, u32), cap: (u32, u32), scale_permille: u32) -> (u32, u32) {
    let (w, h) = if (monitor.1 > monitor.0) != (cap.1 > cap.0) {
        (cap.1, cap.0)
    } else {
        cap
    };
    let s = u64::from(scale_permille.clamp(100, 1000));
    // Rounded to a multiple of 4 so ladder steps give tidy sizes (1920x1200 at 834 -> 1600x1000).
    let scale = |v: u32, min: u32| {
        if s >= 1000 {
            return v.max(min);
        }
        let scaled = (u64::from(v) * s + 2_000) / 4_000 * 4;
        u32::try_from(scaled).unwrap_or(u32::MAX).max(min)
    };
    let min = if h > w {
        (MIN_CAP.1, MIN_CAP.0)
    } else {
        MIN_CAP
    };
    (scale(w, min.0), scale(h, min.1))
}

#[cfg(test)]
mod tests {
    use super::*;

    fn req(preset: QualityPreset) -> QualitySettings {
        QualitySettings {
            preset,
            max_width: 0,
            max_height: 0,
            fps: 0,
            bitrate_kbps: None,
        }
    }

    #[test]
    fn presets_resolve_to_documented_targets() {
        let d = QualityTarget::host_default((1920, 1200), 60);
        let speed = QualityTarget::resolve(&req(QualityPreset::Speed), d);
        assert_eq!(speed.max_size, (1280, 720));
        assert_eq!(speed.fps, 60);
        assert_eq!(speed.bitrate, BitrateMode::Adaptive { ceiling_pct: 60 });
        let balanced = QualityTarget::resolve(&req(QualityPreset::Balanced), d);
        assert_eq!(balanced.max_size, (1920, 1200));
        assert_eq!(balanced.bitrate, BitrateMode::Adaptive { ceiling_pct: 100 });
        let quality = QualityTarget::resolve(&req(QualityPreset::Quality), d);
        assert_eq!(quality.max_size, (3840, 2160));
        assert_eq!(quality.bitrate, BitrateMode::Adaptive { ceiling_pct: 160 });
        // Presets ignore explicit fields.
        let mut r = req(QualityPreset::Speed);
        r.max_width = 7000;
        r.bitrate_kbps = Some(1);
        assert_eq!(QualityTarget::resolve(&r, d), speed);
    }

    #[test]
    fn custom_uses_fields_with_zero_meaning_default() {
        let d = QualityTarget::host_default((1920, 1200), 60);
        let c = QualityTarget::resolve(&req(QualityPreset::Custom), d);
        assert_eq!((c.max_size, c.fps), ((1920, 1200), 60));
        assert_eq!(c.bitrate, BitrateMode::Adaptive { ceiling_pct: 100 });
        let c = QualityTarget::resolve(
            &QualitySettings {
                preset: QualityPreset::Custom,
                max_width: 1600,
                max_height: 900,
                fps: 30,
                bitrate_kbps: Some(4000),
            },
            d,
        );
        assert_eq!((c.max_size, c.fps), ((1600, 900), 30));
        assert_eq!(c.bitrate, BitrateMode::Fixed(4000));
    }

    #[test]
    fn hostile_custom_values_are_clamped() {
        let d = QualityTarget::host_default((1920, 1200), 60);
        let c = QualityTarget::resolve(
            &QualitySettings {
                preset: QualityPreset::Custom,
                max_width: u32::MAX,
                max_height: 1,
                fps: u32::MAX,
                bitrate_kbps: Some(u32::MAX),
            },
            d,
        );
        assert_eq!(c.max_size, (7680, 180));
        assert_eq!(c.fps, 120);
        assert_eq!(c.bitrate, BitrateMode::Fixed(200_000));
        let c = QualityTarget::resolve(
            &QualitySettings {
                preset: QualityPreset::Custom,
                max_width: 1,
                max_height: 1,
                fps: 1,
                bitrate_kbps: Some(0),
            },
            d,
        );
        assert_eq!(c.max_size, (320, 180));
        assert_eq!(c.fps, 1);
        assert_eq!(c.bitrate, BitrateMode::Fixed(100));
        // A silly host default is clamped too.
        assert_eq!(QualityTarget::host_default((0, 0), 0).max_size, (320, 180));
        assert_eq!(QualityTarget::host_default((0, 0), 0).fps, 1);
    }

    #[test]
    fn capture_cap_follows_orientation_and_scale() {
        assert_eq!(capture_cap((3840, 2160), (1920, 1200), 1000), (1920, 1200));
        // Portrait monitor: portrait cap.
        assert_eq!(capture_cap((2160, 3840), (1920, 1200), 1000), (1200, 1920));
        assert_eq!(capture_cap((3840, 2160), (1920, 1200), 500), (960, 600));
        // Never below the minimum, whatever the scale.
        assert_eq!(capture_cap((3840, 2160), (1280, 720), 0), (320, 180));
        assert_eq!(capture_cap((1080, 1920), (1280, 720), 100), (180, 320));
        // Square monitor keeps the cap as given.
        assert_eq!(capture_cap((1000, 1000), (1920, 1200), 1000), (1920, 1200));
        // The ladder's steps give tidy sizes.
        assert_eq!(capture_cap((3840, 2160), (1920, 1200), 834), (1600, 1000));
        assert_eq!(capture_cap((3840, 2160), (1920, 1200), 667), (1280, 800));
        assert_eq!(capture_cap((3840, 2160), (1920, 1200), 534), (1024, 640));
        assert_eq!(capture_cap((1920, 1080), (1920, 1200), 834), (1600, 1000));
    }
}
