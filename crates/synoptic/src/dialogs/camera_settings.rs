//! Camera Settings dialog (440 w): stream protocol, direct RTSP URL, Live
//! View stream profile. (The reference's fisheye dewarp / mount / lens
//! tables stay in `ss_config` for compatibility but have no UI: nothing in
//! this client dewarps.)
//!
//! Values are written into the active profile's camera tables with the
//! reference app's key names and value strings (see `ss_config::ProfileState`):
//! a value equal to its default is *removed* from the table rather than
//! written, exactly as the reference does.

use std::rc::Rc;

use gtk::prelude::*;
use ss_config::{ProfileState, StreamProfile};

use crate::dialogs::shell;
use crate::state::{Change, Store};
use crate::widgets::controls::label;

/// `(config value, label)` in dropdown order. Values are the reference's
/// `camera_protocols` strings (`ss_config::STREAM_PROTOCOLS`).
pub const PROTOCOLS: &[(&str, &str)] = &[
    ("auto", "Auto (WebSocket)"),
    ("websocket", "WebSocket"),
    ("rtsp", "RTSP"),
    ("rtsp_over_http", "RTSP over HTTP"),
    ("multicast", "Multicast"),
    ("mjpeg", "MJPEG"),
    ("direct", "Direct URL"),
];

const PROTOCOL_DEFAULT: &str = "auto";
const PROFILE_DEFAULT: &str = "camera";

/// What the dialog edits for one camera.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Values {
    pub protocol: String,
    pub direct_url: String,
    pub profile: String,
}

impl Default for Values {
    fn default() -> Self {
        Values {
            protocol: PROTOCOL_DEFAULT.into(),
            direct_url: String::new(),
            profile: PROFILE_DEFAULT.into(),
        }
    }
}

impl Values {
    /// Read camera `id` from a profile's tables; unknown strings fall back
    /// to the defaults (a hand-edited `config.toml`).
    pub fn read(state: &ProfileState, id: u32) -> Values {
        let pick = |v: Option<&String>, allowed: &[&str], default: &str| -> String {
            v.filter(|s| allowed.contains(&s.as_str()))
                .cloned()
                .unwrap_or_else(|| default.to_string())
        };
        let protocols: Vec<&str> = PROTOCOLS.iter().map(|(v, _)| *v).collect();
        let profiles: Vec<&str> = StreamProfile::ALL.iter().map(|(p, _)| p.as_str()).collect();
        Values {
            protocol: pick(
                state.camera_protocols.get(&id),
                &protocols,
                PROTOCOL_DEFAULT,
            ),
            direct_url: state.camera_overrides.get(&id).cloned().unwrap_or_default(),
            profile: pick(
                state.camera_live_view_stream_profiles.get(&id),
                &profiles,
                PROFILE_DEFAULT,
            ),
        }
    }

    pub fn validate(&self) -> Result<(), String> {
        if self.protocol == "direct" {
            let url = self.direct_url.trim();
            if url.is_empty() {
                return Err("Enter the direct RTSP URL.".into());
            }
            let lower = url.to_ascii_lowercase();
            if !(lower.starts_with("rtsp://") || lower.starts_with("rtsps://")) {
                return Err("The direct URL must start with rtsp:// or rtsps://.".into());
            }
        }
        Ok(())
    }

    /// Write camera `id`'s tables; defaults remove the entry. The fisheye
    /// tables are left untouched.
    pub fn write(&self, state: &mut ProfileState, id: u32) {
        set_or_remove(
            &mut state.camera_protocols,
            id,
            &self.protocol,
            PROTOCOL_DEFAULT,
        );
        if self.protocol == "direct" {
            state
                .camera_overrides
                .insert(id, self.direct_url.trim().to_string());
        } else {
            state.camera_overrides.remove(&id);
        }
        set_or_remove(
            &mut state.camera_live_view_stream_profiles,
            id,
            &self.profile,
            PROFILE_DEFAULT,
        );
    }
}

fn set_or_remove(
    map: &mut std::collections::BTreeMap<u32, String>,
    id: u32,
    value: &str,
    default: &str,
) {
    if value.is_empty() || value == default {
        map.remove(&id);
    } else {
        map.insert(id, value.to_string());
    }
}

fn index_of(options: &[(&str, &str)], value: &str) -> u32 {
    options.iter().position(|(v, _)| *v == value).unwrap_or(0) as u32
}

fn dropdown(options: &[(&str, &str)], value: &str) -> gtk::DropDown {
    let labels: Vec<&str> = options.iter().map(|(_, l)| *l).collect();
    let d = gtk::DropDown::from_strings(&labels);
    d.set_selected(index_of(options, value));
    d.set_valign(gtk::Align::Center);
    d
}

/// Label on the left, control on the right.
fn row(text: &str, control: &impl IsA<gtk::Widget>) -> gtk::Box {
    let b = gtk::Box::new(gtk::Orientation::Horizontal, 12);
    let l = label(text, &["row-label"]);
    l.set_hexpand(true);
    l.set_valign(gtk::Align::Center);
    b.append(&l);
    b.append(control);
    b
}

/// The profile whose camera tables we edit: the connected one, else the default.
fn active_profile_name(store: &Store) -> String {
    store
        .get()
        .connection
        .as_ref()
        .map(|c| c.profile.clone())
        .unwrap_or_else(|| store.config().general.default_profile.clone())
}

/// Open Camera Settings for camera `id`.
pub fn open(parent: &impl IsA<gtk::Window>, store: &Rc<Store>, id: i64) {
    let camera = store
        .get()
        .camera(id)
        .map(|c| (c.name.clone(), c.model.clone()));
    let Some((name, model)) = camera else {
        store.toast(format!("Camera {id} is not in the camera list"));
        return;
    };
    let profile_name = active_profile_name(store);
    let cam_id = id.max(0) as u32;
    let values = store
        .config()
        .profiles
        .get(&profile_name)
        .map(|p| Values::read(&p.state, cam_id))
        .unwrap_or_default();

    let dialog = shell::present(
        parent,
        440,
        "tune",
        &name,
        Some(&format!("{model} · ID {id}")),
    );
    let body = &dialog.body;

    let protocol = dropdown(PROTOCOLS, &values.protocol);
    body.append(&row("Stream protocol", &protocol));

    let url = gtk::Entry::new();
    url.add_css_class("mono");
    url.set_text(&values.direct_url);
    url.set_placeholder_text(Some("rtsp://user:pass@host:554/stream"));
    let url_field = gtk::Box::new(gtk::Orientation::Vertical, 4);
    url_field.append(&label("Direct RTSP URL", &["field-label"]));
    url_field.append(&url);
    url_field.set_visible(values.protocol == "direct");
    body.append(&url_field);
    {
        let url_field = url_field.clone();
        protocol.connect_selected_notify(move |d| {
            url_field.set_visible(PROTOCOLS[d.selected() as usize].0 == "direct");
        });
    }

    let profile_options: Vec<(&str, &str)> = StreamProfile::ALL
        .iter()
        .map(|(p, l)| (p.as_str(), *l))
        .collect();
    let profile = dropdown(&profile_options, &values.profile);
    body.append(&row("Live View stream profile", &profile));

    let error = label("", &["error-text"]);
    error.set_wrap(true);
    error.set_visible(false);
    body.append(&error);

    let cancel = gtk::Button::with_label("Cancel");
    {
        let window = dialog.window.clone();
        cancel.connect_clicked(move |_| window.close());
    }
    let apply = gtk::Button::with_label("Apply");
    apply.add_css_class("primary");
    {
        let store = store.clone();
        let window = dialog.window.clone();
        apply.connect_clicked(move |_| {
            let v = Values {
                protocol: PROTOCOLS[protocol.selected() as usize].0.to_string(),
                direct_url: url.text().trim().to_string(),
                profile: profile_options[profile.selected() as usize].0.to_string(),
            };
            if let Err(e) = v.validate() {
                error.set_text(&e);
                error.set_visible(true);
                return;
            }
            let saved = {
                let mut cfg = store.config();
                match cfg.profiles.get_mut(&profile_name) {
                    Some(p) => {
                        v.write(&mut p.state, cam_id);
                        true
                    }
                    None => false,
                }
            };
            if !saved {
                error.set_text("No connection profile to save these settings to.");
                error.set_visible(true);
                return;
            }
            store.save_soon();
            store.emit(&Change::CameraSettings(id));
            store.toast(format!("Settings applied · {name}"));
            window.close();
        });
    }
    dialog.set_footer(&[&cancel, &apply]);
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn defaults_are_not_written() {
        let mut st = ProfileState::default();
        Values::default().write(&mut st, 7);
        assert!(st.camera_protocols.is_empty());
        assert!(st.camera_overrides.is_empty());
        assert!(st.camera_live_view_stream_profiles.is_empty());
    }

    #[test]
    fn round_trip_uses_reference_strings() {
        let v = Values {
            protocol: "direct".into(),
            direct_url: " rtsp://nas/stream ".into(),
            profile: "balanced".into(),
        };
        let mut st = ProfileState::default();
        st.camera_dewarp.insert(59, "ptz".into());
        v.write(&mut st, 59);
        assert_eq!(st.camera_protocols[&59], "direct");
        assert_eq!(st.camera_overrides[&59], "rtsp://nas/stream");
        assert_eq!(st.camera_live_view_stream_profiles[&59], "balanced");
        // the reference's fisheye tables are preserved, not edited
        assert_eq!(st.camera_dewarp[&59], "ptz");
        let back = Values::read(&st, 59);
        assert_eq!(back.direct_url, "rtsp://nas/stream");
        assert_eq!(back.protocol, "direct");
        assert_eq!(back.profile, "balanced");
    }

    #[test]
    fn switching_away_from_direct_drops_the_override() {
        let mut st = ProfileState::default();
        st.camera_overrides.insert(1, "rtsp://x".into());
        st.camera_protocols.insert(1, "direct".into());
        let v = Values {
            protocol: "rtsp".into(),
            ..Values::default()
        };
        v.write(&mut st, 1);
        assert!(st.camera_overrides.is_empty());
        assert_eq!(st.camera_protocols[&1], "rtsp");
    }

    #[test]
    fn unknown_config_strings_fall_back() {
        let mut st = ProfileState::default();
        st.camera_protocols.insert(1, "carrier-pigeon".into());
        st.camera_live_view_stream_profiles
            .insert(1, "ultra".into());
        let v = Values::read(&st, 1);
        assert_eq!(v.protocol, "auto");
        assert_eq!(v.profile, "camera");
    }

    #[test]
    fn direct_needs_an_rtsp_url() {
        let mut v = Values {
            protocol: "direct".into(),
            ..Values::default()
        };
        assert!(v.validate().is_err());
        v.direct_url = "http://x".into();
        assert!(v.validate().is_err());
        v.direct_url = "RTSP://x/live".into();
        assert!(v.validate().is_ok());
    }
}
