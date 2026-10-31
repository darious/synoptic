//! The Settings page's registry: every runtime-tunable constant the
//! reference exposes (`surveillance/settings_registry.py`), with its
//! default, bounds and options, and the overrides kept in
//! `[setting_overrides]`, `[setting_overrides_bool]` and
//! `[setting_overrides_choice]`.
//!
//! Only changed settings are stored; resetting one *drops* its override
//! rather than writing the default back, so a later release that retunes
//! a constant reaches everyone who ever pressed Reset.

use crate::model::{AppConfig, StreamProfile};

/// How a setting is edited and stored.
#[derive(Debug, Clone, Copy, PartialEq)]
pub enum SettingKind {
    /// A spinner; stored in `[setting_overrides]`.
    Number {
        default: f64,
        min: f64,
        max: f64,
        step: f64,
        /// Decimals offered; 0 = whole numbers only.
        digits: u8,
    },
    /// A switch; stored in `[setting_overrides_bool]`.
    Bool { default: bool },
    /// A dropdown of `(value, label)` in display order; stored in
    /// `[setting_overrides_choice]`.
    Choice {
        default: &'static str,
        options: &'static [(&'static str, &'static str)],
    },
}

/// One registered setting.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct SettingDef {
    /// The TOML key.
    pub key: &'static str,
    /// Settings-page subsection title.
    pub section: &'static str,
    pub label: &'static str,
    pub tooltip: &'static str,
    pub kind: SettingKind,
}

/// A setting's value, typed by its kind.
#[derive(Debug, Clone, PartialEq)]
pub enum SettingValue {
    Number(f64),
    Bool(bool),
    Choice(String),
}

/// Why a value was refused.
#[derive(Debug, Clone, PartialEq, thiserror::Error)]
pub enum SettingError {
    #[error("unknown setting {0:?}")]
    UnknownKey(String),
    #[error("setting {key:?} takes a {expected}, not a {given}")]
    WrongKind {
        key: &'static str,
        expected: &'static str,
        given: &'static str,
    },
    #[error("setting {key:?}: {value} is outside {min}..={max}")]
    OutOfRange {
        key: &'static str,
        value: f64,
        min: f64,
        max: f64,
    },
    #[error("setting {key:?}: {value:?} is not one of its options")]
    NotAnOption { key: &'static str, value: String },
}

const SECTION_LIVE_VIEW: &str = "Live View settings";
const SECTION_PLAYER: &str = "Media player settings";
const SECTION_TIMELINE: &str = "Timeline settings";
const SECTION_EVENTS: &str = "Event settings";
const SECTION_RECEIPT: &str = "POS receipt overlay";

/// Settings-page subsections in display order.
pub const SECTIONS: &[&str] = &[
    SECTION_LIVE_VIEW,
    SECTION_PLAYER,
    SECTION_TIMELINE,
    SECTION_EVENTS,
    SECTION_RECEIPT,
];

const STREAM_PROFILE_OPTIONS: &[(&str, &str)] = &[
    ("camera", "Use camera settings"),
    ("high", "High quality"),
    ("balanced", "Balanced"),
    ("low", "Low bandwidth"),
];

const fn number(default: f64, min: f64, max: f64, step: f64, digits: u8) -> SettingKind {
    SettingKind::Number {
        default,
        min,
        max,
        step,
        digits,
    }
}

/// Every setting, in the reference's order (sections, then within a
/// section numbers, switches, dropdowns).
pub const SETTINGS: &[SettingDef] = &[
    SettingDef {
        key: "live_view_stream_profile",
        section: SECTION_LIVE_VIEW,
        label: "Live View stream profile",
        tooltip: "Which Surveillance Station stream profile Live View plays for each camera. \
                  \"Use camera settings\" follows each camera's own choice in Camera Settings \
                  (right-click it in the sidebar), and otherwise its Live View setting in \
                  Surveillance Station. Any other value applies to every camera. WebSocket \
                  streams only: RTSP always uses the camera's Live View setting in \
                  Surveillance Station.",
        kind: SettingKind::Choice {
            default: "camera",
            options: STREAM_PROFILE_OPTIONS,
        },
    },
    SettingDef {
        key: "cache_seconds_default",
        section: SECTION_PLAYER,
        label: "Default camera cache size (seconds)",
        tooltip: "Demuxer cache target for a plain RTSP camera stream. Keep small but not \
                  zero to absorb normal scheduling jitter without audible micro-cuts.",
        kind: number(0.5, 0.0, 10.0, 0.1, 2),
    },
    SettingDef {
        key: "cache_seconds_muxed_audio",
        section: SECTION_PLAYER,
        label: "Audio-camera cache size (seconds)",
        tooltip: "Demuxer cache target for a live-piped stream muxed with real audio. Keep \
                  small but not zero to absorb normal scheduling jitter without audible \
                  micro-cuts.",
        kind: number(0.5, 0.0, 10.0, 0.1, 2),
    },
    SettingDef {
        key: "cache_seconds_low_latency",
        section: SECTION_PLAYER,
        label: "Silent-camera cache size (seconds)",
        tooltip: "Demuxer cache target for WebSocket pipes used by cameras without audio \
                  track. Will provide least possible latency when set to 0.",
        kind: number(0.0, 0.0, 10.0, 0.1, 2),
    },
    SettingDef {
        key: "cache_high_speed_max_seconds",
        section: SECTION_PLAYER,
        label: "Max high-speed History cache (seconds)",
        tooltip: "Extra cache on top of a streaming profile's own baseline at History's \
                  fastest (100x) playback speed. DSM delivers frames proportionally faster \
                  at high speed, so a bigger buffer here keeps a fast rewind from outrunning \
                  its cache. Cache growth between 2x and 100x is exponential.",
        kind: number(8.0, 1.0, 60.0, 1.0, 2),
    },
    SettingDef {
        key: "demuxer_max_bytes_mib",
        section: SECTION_PLAYER,
        label: "Demuxer byte cap (MiB)",
        tooltip: "Byte-size cap on the demuxer cache, same for every streaming profile above \
                  that runs one. Player reads ahead by whichever of this and the \
                  seconds-based cache targets above is larger, so too small a value here can \
                  bottleneck a cache otherwise sized generously (e.g. the silent-camera cache \
                  at a high History speed).",
        kind: number(32.0, 1.0, 512.0, 1.0, 0),
    },
    SettingDef {
        key: "demuxer_max_back_bytes_mib",
        section: SECTION_PLAYER,
        label: "Demuxer back buffer cap (MiB)",
        tooltip: "How much already-played video each player keeps for seeking backwards. \
                  Live View never uses it, so a small value saves a lot of memory on a full \
                  grid. Larger values only help the Recordings player's skip-back, which \
                  otherwise reads the recording again.",
        kind: number(1.0, 0.0, 512.0, 1.0, 0),
    },
    SettingDef {
        key: "osd_enabled",
        section: SECTION_PLAYER,
        label: "Show stream cache details overlaid on video",
        tooltip: "Draws a small live readout (cache depth, target, and effective playback \
                  speed) in the corner of each video: useful for diagnosing buffering or \
                  speed-correction behavior, not needed for normal use.",
        kind: SettingKind::Bool { default: false },
    },
    SettingDef {
        key: "max_speed_slot_product",
        section: SECTION_TIMELINE,
        label: "Max limit for (playback speed X number of slots)",
        tooltip: "Ceiling on (playback speed X active grid slots) offered in the History \
                  speed dropdown. DSM delivers that many times more video per second, on \
                  every slot at once, so too high a product across a full grid can overload \
                  the app. Higher speeds are greyed out on layouts with more slots to stay \
                  under this budget; 1x stays on offer however low it goes.",
        kind: number(100.0, 1.0, 1000.0, 10.0, 2),
    },
    SettingDef {
        key: "force_legacy_events",
        section: SECTION_EVENTS,
        label: "Decode events as before Surveillance Station 9.3 (testing)",
        tooltip: "Reads events from the recording-interval bitmask the way Surveillance \
                  Station before 9.3 requires, even where the Event Center is available. For \
                  testing that path on a newer NAS: on 9.3 and later some event types come \
                  out wrong this way. Takes effect at the next login.",
        kind: SettingKind::Bool { default: false },
    },
    SettingDef {
        key: "receipt_scale_percent",
        section: SECTION_RECEIPT,
        label: "Receipt text size (%)",
        tooltip: "Size of the POS receipt shown over a cashier camera, as a percentage of \
                  the normal overlay text. Lower it for small tiles in a 3x3 or 4x4 grid.",
        kind: number(100.0, 25.0, 400.0, 5.0, 0),
    },
    SettingDef {
        key: "receipt_max_lines",
        section: SECTION_RECEIPT,
        label: "Receipt maximum lines",
        tooltip: "Longest receipt drawn in full. A longer one keeps its first and last lines \
                  (header, total, payment) and folds the items in between into one line.",
        kind: number(14.0, 3.0, 40.0, 1.0, 0),
    },
    SettingDef {
        key: "receipt_seconds",
        section: SECTION_RECEIPT,
        label: "Receipt time on screen (seconds)",
        tooltip: "How long a receipt stays over the video. Receipts older than this when \
                  they reach the app are not shown.",
        kind: number(60.0, 5.0, 600.0, 5.0, 0),
    },
    SettingDef {
        key: "receipt_enabled",
        section: SECTION_RECEIPT,
        label: "Show POS receipts over the paired camera",
        tooltip: "Surveillance Station Transactions: each new receipt from a POS device is \
                  drawn over every Live View slot showing the camera paired with that \
                  device. Live view only.",
        kind: SettingKind::Bool { default: true },
    },
];

/// The definition of `key`, if registered.
pub fn setting(key: &str) -> Option<&'static SettingDef> {
    SETTINGS.iter().find(|s| s.key == key)
}

/// The settings of one section, in display order.
pub fn settings_in(section: &str) -> impl Iterator<Item = &'static SettingDef> {
    SETTINGS.iter().filter(move |s| s.section == section)
}

impl SettingDef {
    /// The value with no override.
    pub fn default_value(&self) -> SettingValue {
        match self.kind {
            SettingKind::Number { default, .. } => SettingValue::Number(default),
            SettingKind::Bool { default } => SettingValue::Bool(default),
            SettingKind::Choice { default, .. } => SettingValue::Choice(default.to_string()),
        }
    }

    /// Check `value` against this setting's kind, range or options.
    pub fn validate(&self, value: &SettingValue) -> Result<(), SettingError> {
        let wrong = |expected, given| SettingError::WrongKind {
            key: self.key,
            expected,
            given,
        };
        match (self.kind, value) {
            (SettingKind::Number { min, max, .. }, SettingValue::Number(n)) => {
                if n.is_finite() && (min..=max).contains(n) {
                    Ok(())
                } else {
                    Err(SettingError::OutOfRange {
                        key: self.key,
                        value: *n,
                        min,
                        max,
                    })
                }
            }
            (SettingKind::Bool { .. }, SettingValue::Bool(_)) => Ok(()),
            (SettingKind::Choice { options, .. }, SettingValue::Choice(c)) => {
                if options.iter().any(|(v, _)| v == c) {
                    Ok(())
                } else {
                    Err(SettingError::NotAnOption {
                        key: self.key,
                        value: c.clone(),
                    })
                }
            }
            (SettingKind::Number { .. }, v) => Err(wrong("number", kind_name(v))),
            (SettingKind::Bool { .. }, v) => Err(wrong("switch", kind_name(v))),
            (SettingKind::Choice { .. }, v) => Err(wrong("choice", kind_name(v))),
        }
    }
}

fn kind_name(v: &SettingValue) -> &'static str {
    match v {
        SettingValue::Number(_) => "number",
        SettingValue::Bool(_) => "switch",
        SettingValue::Choice(_) => "choice",
    }
}

impl AppConfig {
    /// The effective value of `key`: its override when one is stored and
    /// valid for the setting, else its default. `None` for an unknown key.
    pub fn setting(&self, key: &str) -> Option<SettingValue> {
        let def = setting(key)?;
        let stored = match def.kind {
            SettingKind::Number { .. } => self
                .setting_overrides
                .get(key)
                .map(|n| SettingValue::Number(*n)),
            SettingKind::Bool { .. } => self
                .setting_overrides_bool
                .get(key)
                .map(|b| SettingValue::Bool(*b)),
            SettingKind::Choice { .. } => self
                .setting_overrides_choice
                .get(key)
                .map(|c| SettingValue::Choice(c.clone())),
        };
        Some(match stored {
            Some(v) if def.validate(&v).is_ok() => v,
            _ => def.default_value(),
        })
    }

    /// A numeric setting's effective value; the default when `key` is not
    /// a numeric setting.
    pub fn number_setting(&self, key: &str) -> f64 {
        match self.setting(key) {
            Some(SettingValue::Number(n)) => n,
            _ => 0.0,
        }
    }

    /// An on/off setting's effective value; `false` when `key` is not one.
    pub fn bool_setting(&self, key: &str) -> bool {
        matches!(self.setting(key), Some(SettingValue::Bool(true)))
    }

    /// A dropdown setting's effective value; `""` when `key` is not one.
    pub fn choice_setting(&self, key: &str) -> String {
        match self.setting(key) {
            Some(SettingValue::Choice(c)) => c,
            _ => String::new(),
        }
    }

    /// The effective Live View stream profile from the Settings page.
    pub fn live_view_stream_profile(&self) -> StreamProfile {
        self.choice_setting("live_view_stream_profile")
            .parse()
            .unwrap_or_default()
    }

    /// Store an override for `key` after validating it.
    pub fn set_setting(&mut self, key: &str, value: SettingValue) -> Result<(), SettingError> {
        let def = setting(key).ok_or_else(|| SettingError::UnknownKey(key.to_string()))?;
        def.validate(&value)?;
        match value {
            SettingValue::Number(n) => {
                self.setting_overrides.insert(def.key.to_string(), n);
            }
            SettingValue::Bool(b) => {
                self.setting_overrides_bool.insert(def.key.to_string(), b);
            }
            SettingValue::Choice(c) => {
                self.setting_overrides_choice.insert(def.key.to_string(), c);
            }
        }
        Ok(())
    }

    /// Drop `key`'s override so its default applies again. Unknown keys
    /// are ignored.
    pub fn reset_setting(&mut self, key: &str) {
        self.setting_overrides.remove(key);
        self.setting_overrides_bool.remove(key);
        self.setting_overrides_choice.remove(key);
    }

    /// Drop every registered setting's override. Overrides for keys this
    /// build does not know (a newer reference's) are left in place.
    pub fn reset_all_settings(&mut self) {
        for def in SETTINGS {
            self.reset_setting(def.key);
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn keys_in(section: &str) -> Vec<&'static str> {
        settings_in(section).map(|s| s.key).collect()
    }

    #[test]
    fn sections_hold_the_reference_keys() {
        assert_eq!(keys_in("Live View settings"), ["live_view_stream_profile"]);
        assert_eq!(
            keys_in("Media player settings"),
            [
                "cache_seconds_default",
                "cache_seconds_muxed_audio",
                "cache_seconds_low_latency",
                "cache_high_speed_max_seconds",
                "demuxer_max_bytes_mib",
                "demuxer_max_back_bytes_mib",
                "osd_enabled",
            ]
        );
        assert_eq!(keys_in("Timeline settings"), ["max_speed_slot_product"]);
        assert_eq!(keys_in("Event settings"), ["force_legacy_events"]);
        assert_eq!(
            keys_in("receipt overlay".replace("receipt", "POS receipt").as_str()),
            [
                "receipt_scale_percent",
                "receipt_max_lines",
                "receipt_seconds",
                "receipt_enabled"
            ]
        );
        let total: usize = SECTIONS.iter().map(|s| keys_in(s).len()).sum();
        assert_eq!(total, SETTINGS.len());
    }

    #[test]
    fn stream_profile_options_match_the_enum() {
        let SettingKind::Choice { default, options } =
            setting("live_view_stream_profile").unwrap().kind
        else {
            panic!("not a choice");
        };
        assert_eq!(default, "camera");
        let values: Vec<_> = options.iter().map(|(v, _)| *v).collect();
        assert_eq!(values, ["camera", "high", "balanced", "low"]);
        for (value, label) in options {
            let p: StreamProfile = value.parse().unwrap();
            assert_eq!(p.as_str(), *value);
            assert!(StreamProfile::ALL.contains(&(p, *label)));
        }
    }

    #[test]
    fn whole_number_defaults_sit_on_a_tick() {
        for def in SETTINGS {
            if let SettingKind::Number {
                default,
                min,
                step,
                digits: 0,
                ..
            } = def.kind
            {
                let ticks = (default - min) / step;
                assert_eq!(ticks, ticks.trunc(), "{}", def.key);
            }
        }
    }

    #[test]
    fn every_setting_has_a_label_and_tooltip_and_a_default_in_range() {
        for def in SETTINGS {
            assert!(!def.label.trim().is_empty(), "{}", def.key);
            assert!(!def.tooltip.trim().is_empty(), "{}", def.key);
            assert_eq!(def.validate(&def.default_value()), Ok(()), "{}", def.key);
        }
    }

    #[test]
    fn get_set_reset_round_trip() {
        let mut cfg = AppConfig::default();
        assert_eq!(cfg.number_setting("cache_seconds_default"), 0.5);
        assert_eq!(cfg.number_setting("demuxer_max_bytes_mib"), 32.0);
        assert!(!cfg.bool_setting("osd_enabled"));
        assert!(cfg.bool_setting("receipt_enabled"));
        assert_eq!(cfg.live_view_stream_profile(), StreamProfile::Camera);

        cfg.set_setting("cache_seconds_default", SettingValue::Number(3.3))
            .unwrap();
        assert_eq!(cfg.number_setting("cache_seconds_default"), 3.3);
        assert_eq!(cfg.setting_overrides["cache_seconds_default"], 3.3);
        cfg.set_setting("osd_enabled", SettingValue::Bool(true))
            .unwrap();
        assert!(cfg.bool_setting("osd_enabled"));
        cfg.set_setting(
            "live_view_stream_profile",
            SettingValue::Choice("low".into()),
        )
        .unwrap();
        assert_eq!(cfg.live_view_stream_profile(), StreamProfile::Low);

        cfg.reset_setting("cache_seconds_default");
        assert!(!cfg.setting_overrides.contains_key("cache_seconds_default"));
        assert_eq!(cfg.number_setting("cache_seconds_default"), 0.5);

        cfg.setting_overrides
            .insert("from_a_newer_release".into(), 1.0);
        cfg.reset_all_settings();
        assert!(cfg.setting_overrides_bool.is_empty());
        assert!(cfg.setting_overrides_choice.is_empty());
        assert_eq!(
            cfg.setting_overrides.keys().collect::<Vec<_>>(),
            ["from_a_newer_release"]
        );
    }

    #[test]
    fn bad_values_are_refused() {
        let mut cfg = AppConfig::default();
        assert_eq!(
            cfg.set_setting("nope", SettingValue::Bool(true)),
            Err(SettingError::UnknownKey("nope".into()))
        );
        assert!(matches!(
            cfg.set_setting("cache_seconds_default", SettingValue::Number(11.0)),
            Err(SettingError::OutOfRange { .. })
        ));
        assert!(matches!(
            cfg.set_setting("cache_seconds_default", SettingValue::Number(f64::NAN)),
            Err(SettingError::OutOfRange { .. })
        ));
        assert!(matches!(
            cfg.set_setting("osd_enabled", SettingValue::Number(1.0)),
            Err(SettingError::WrongKind { .. })
        ));
        assert!(matches!(
            cfg.set_setting(
                "live_view_stream_profile",
                SettingValue::Choice("ultra".into())
            ),
            Err(SettingError::NotAnOption { .. })
        ));
        // A hand-edited override outside the range reads as the default.
        cfg.setting_overrides
            .insert("receipt_seconds".into(), 9999.0);
        assert_eq!(cfg.number_setting("receipt_seconds"), 60.0);
        cfg.setting_overrides_choice
            .insert("live_view_stream_profile".into(), "ultra".into());
        assert_eq!(cfg.live_view_stream_profile(), StreamProfile::Camera);
    }
}
