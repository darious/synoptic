//! File-level behaviour: the README's example, round trips, unknown
//! keys, lenient loading ported from the reference's `test_config.py`,
//! and the atomic save.

use std::collections::BTreeMap;
use std::fs;

use ss_config::{
    AppConfig, EventTypeHistory, MIN_POLL_INTERVAL, Profile, SearchFilters, SearchPage, Theme,
    TimePreset, load_from, load_or_salvage, save_to,
};
use toml::{Table, Value};

/// The README's annotated example, with its commented-out lines enabled.
const README_EXAMPLE: &str = include_str!("readme_example.toml");

fn parse(text: &str) -> AppConfig {
    AppConfig::from_toml_str(text).expect("valid TOML")
}

fn table(text: &str) -> Table {
    toml::from_str(text).expect("valid TOML")
}

// ---------------------------------------------------------------- README

#[test]
fn readme_example_loads_every_section() {
    let cfg = parse(README_EXAMPLE);
    assert_eq!(cfg.general.default_profile, "home-nas");
    assert_eq!(cfg.general.theme, Theme::Auto);
    assert_eq!(cfg.general.poll_interval_transactions, 5);
    assert_eq!(
        cfg.general.snapshot_dir,
        "/home/user/.local/share/surveillance-station/snapshots"
    );
    assert_eq!(cfg.session.grid_layout, "2x2");
    assert_eq!(cfg.session.search_from_time, "2026-02-01T00:00:00");
    assert_eq!(cfg.session.search_time_preset, "today");
    assert_eq!(cfg.setting_overrides["cache_seconds_default"], 1.0);
    assert_eq!(cfg.setting_overrides["demuxer_max_bytes_mib"], 64.0);
    assert!(cfg.setting_overrides_bool["osd_enabled"]);
    assert!(cfg.setting_overrides_bool["force_legacy_events"]);
    assert_eq!(
        cfg.setting_overrides_choice["live_view_stream_profile"],
        "low"
    );

    let p = &cfg.profiles["home-nas"];
    assert_eq!(p.host, "192.168.1.100");
    assert_eq!(p.port, 5001);
    assert!(p.https);
    assert!(!p.verify_ssl);
    assert_eq!(p.base_url(), "https://192.168.1.100:5001");
    assert_eq!(p.state.search_camera_ids, vec![1, 3]);
    assert_eq!(p.state.layout_cameras["2x2"], vec![1, 3, 0, 5]);
    assert_eq!(
        p.state.layout_cameras["3x3"],
        vec![1, 3, 7, 0, 5, 8, 2, 0, 0]
    );
    assert_eq!(
        p.state.camera_overrides[&5],
        "rtsp://admin:password@192.168.1.50:554/h265Preview_01_main"
    );
    assert_eq!(p.state.camera_volume[&5], 40);
    assert!(!p.state.camera_muted[&5]);
    assert_eq!(
        p.state.legacy_event_type_history[&5],
        EventTypeHistory {
            types: vec![(3, 0), (33554435, 2)],
            checked_until: 1771200000,
        }
    );
    assert_eq!(p.state.camera_protocols[&5], "direct");
    assert_eq!(p.state.camera_live_view_stream_profiles[&5], "high");
    assert_eq!(p.state.camera_dewarp[&5], "ptz");
    assert_eq!(p.state.camera_fisheye_lens[&5], "0.5,0.5,0.48,180");
    assert_eq!(p.state.camera_fisheye_mount[&5], "wall");
    assert!(cfg.extra.is_empty());
    assert!(cfg.general.extra.is_empty());
    assert!(cfg.session.extra.is_empty());
    assert!(p.extra.is_empty());
}

#[test]
fn readme_example_round_trips_through_save_and_parse() {
    let cfg = parse(README_EXAMPLE);
    let text = cfg.to_toml_string().unwrap();
    let again = parse(&text);
    assert_eq!(cfg, again);
    // And the file itself says the same thing as the README once comments
    // are gone: every README key comes back with the same value. The only
    // additions are the `[session]` keys the reference always writes.
    let saved = table(&text);
    let readme = table(README_EXAMPLE);
    assert_subset(&readme, &saved, "");
    let added: Vec<String> = flat_keys(&saved, "")
        .into_iter()
        .filter(|k| !flat_keys(&readme, "").contains(k))
        .collect();
    assert!(
        added.iter().all(|k| k.starts_with("session.")),
        "unexpected additions: {added:?}"
    );
}

/// Every leaf of `expected` is in `actual` with the same value.
fn assert_subset(expected: &Table, actual: &Table, path: &str) {
    for (k, v) in expected {
        let at = format!("{path}{k}");
        let a = actual
            .get(k)
            .unwrap_or_else(|| panic!("{at} missing from saved file"));
        match (v, a) {
            (Value::Table(e), Value::Table(a)) => assert_subset(e, a, &format!("{at}.")),
            _ => assert_eq!(v, a, "{at}"),
        }
    }
}

fn flat_keys(t: &Table, path: &str) -> Vec<String> {
    let mut out = Vec::new();
    for (k, v) in t {
        let at = format!("{path}{k}");
        match v {
            Value::Table(inner) => out.extend(flat_keys(inner, &format!("{at}."))),
            _ => out.push(at),
        }
    }
    out
}

// ---------------------------------------------------------- unknown keys

#[test]
fn unknown_keys_survive_a_round_trip_at_every_level() {
    let text = r#"
future_top_level = { a = 1, b = "two" }
[general]
default_profile = "nas"
future_general = "kept"
[session]
future_session = [1, 2, 3]
when = 2026-02-01T00:00:00
[future_table]
x = 1
[profiles.nas]
host = "h"
future_profile = true
[profiles.nas.future_camera_table]
5 = "x"
"#;
    let cfg = parse(text);
    assert_eq!(cfg.general.extra["future_general"], Value::from("kept"));
    assert_eq!(
        cfg.profiles["nas"].extra["future_profile"],
        Value::from(true)
    );
    let saved = cfg.to_toml_string().unwrap();
    let out = table(&saved);
    assert_eq!(out["future_top_level"]["b"], Value::from("two"));
    assert_eq!(out["future_table"]["x"], Value::from(1));
    assert_eq!(out["general"]["future_general"], Value::from("kept"));
    assert_eq!(out["session"]["future_session"], Value::from(vec![1, 2, 3]));
    assert!(matches!(out["session"]["when"], Value::Datetime(_)));
    assert_eq!(out["profiles"]["nas"]["future_profile"], Value::from(true));
    assert_eq!(
        out["profiles"]["nas"]["future_camera_table"]["5"],
        Value::from("x")
    );
    assert_eq!(parse(&saved), cfg);
}

// -------------------------------------------------------------- defaults

#[test]
fn defaults_match_the_reference() {
    let cfg = AppConfig::default();
    assert_eq!(cfg.general.default_profile, "");
    assert_eq!(cfg.general.theme, Theme::Auto);
    assert!(cfg.general.sidebar_visible);
    assert!(cfg.general.timeline_visible);
    assert_eq!(cfg.general.dismissed_update_version, "");
    assert_eq!(cfg.general.poll_interval_cameras, 30);
    assert_eq!(cfg.general.poll_interval_alerts, 30);
    assert_eq!(cfg.general.poll_interval_homemode, 60);
    assert_eq!(cfg.general.poll_interval_transactions, 5);
    assert!(
        cfg.general
            .snapshot_dir
            .ends_with("surveillance-station/snapshots")
    );
    assert_eq!(cfg.session.grid_layout, "2x2");
    assert_eq!(cfg.session.last_page, "live");
    assert_eq!(cfg.session.search_time_preset, "");
    assert_eq!(cfg.session.events_search_time_preset, "today");
    assert_eq!(cfg.session.snapshots_search_time_preset, "");
    assert!(!cfg.session.events_search_event_types_match_all);
    assert!(cfg.profiles.is_empty());
    assert!(cfg.setting_overrides.is_empty());

    let p = Profile::default();
    assert_eq!(p.host, "");
    assert_eq!(p.port, 5001);
    assert!(p.https);
    assert!(!p.verify_ssl);
    assert_eq!(p.device_id, "");
    assert!(p.state.is_empty());

    // An empty file is the defaults too.
    assert_eq!(parse(""), cfg);
    assert_eq!(parse("[profiles.x]\n").profiles["x"], Profile::default());
}

#[test]
fn saving_defaults_writes_the_reference_layout() {
    let out = table(&AppConfig::default().to_toml_string().unwrap());
    let general = out["general"].as_table().unwrap();
    for key in [
        "default_profile",
        "poll_interval_cameras",
        "poll_interval_alerts",
        "poll_interval_homemode",
        "poll_interval_transactions",
        "theme",
        "sidebar_visible",
        "timeline_visible",
        "dismissed_update_version",
        "snapshot_dir",
    ] {
        assert!(general.contains_key(key), "general.{key}");
    }
    let session = out["session"].as_table().unwrap();
    for key in [
        "grid_layout",
        "last_page",
        "search_from_time",
        "search_to_time",
        "search_time_preset",
        "events_search_from_time",
        "events_search_to_time",
        "events_search_time_preset",
        "events_search_event_types",
        "events_search_event_types_match_all",
        "snapshots_search_from_time",
        "snapshots_search_to_time",
        "snapshots_search_time_preset",
    ] {
        assert!(session.contains_key(key), "session.{key}");
    }
    assert_eq!(out["general"]["theme"], Value::from("auto"));
    assert_eq!(out["setting_overrides"], Value::Table(Table::new()));
    assert_eq!(out["setting_overrides_bool"], Value::Table(Table::new()));
    assert_eq!(out["setting_overrides_choice"], Value::Table(Table::new()));
    assert_eq!(out["profiles"], Value::Table(Table::new()));
}

#[test]
fn a_profile_writes_only_its_non_empty_camera_tables_and_device_id() {
    let mut cfg = AppConfig::default();
    let mut p = Profile::new("10.0.0.1");
    p.state.camera_protocols.insert(5, "direct".into());
    cfg.add_profile("home", p);
    cfg.add_profile("office", Profile::new("office.example.com"));
    assert_eq!(cfg.general.default_profile, "home");

    let out = table(&cfg.to_toml_string().unwrap());
    let home = out["profiles"]["home"].as_table().unwrap();
    assert_eq!(
        home.keys().collect::<Vec<_>>(),
        ["camera_protocols", "host", "https", "port", "verify_ssl"]
    );
    assert_eq!(home["camera_protocols"]["5"], Value::from("direct"));
    let office = out["profiles"]["office"].as_table().unwrap();
    assert!(!office.contains_key("camera_protocols"));
    assert!(!office.contains_key("device_id"));
    assert!(!out.contains_key("camera_protocols"));

    cfg.profiles.get_mut("office").unwrap().device_id = "tok".into();
    let out = table(&cfg.to_toml_string().unwrap());
    assert_eq!(out["profiles"]["office"]["device_id"], Value::from("tok"));
}

// ------------------------------------------------------- lenient loading

#[test]
fn poll_intervals_are_clamped_and_fall_back() {
    let cfg = parse("[general]\npoll_interval_cameras = 0\n");
    assert_eq!(cfg.general.poll_interval_cameras, MIN_POLL_INTERVAL);
    let cfg = parse("[general]\npoll_interval_alerts = -5\n");
    assert_eq!(cfg.general.poll_interval_alerts, MIN_POLL_INTERVAL);
    let cfg = parse("[general]\npoll_interval_homemode = \"often\"\n");
    assert_eq!(cfg.general.poll_interval_homemode, 60);
    let cfg = parse("[general]\npoll_interval_cameras = \"45\"\n");
    assert_eq!(cfg.general.poll_interval_cameras, 45);
    let cfg = parse("[general]\npoll_interval_cameras = 120\n");
    assert_eq!(cfg.general.poll_interval_cameras, 120);
}

#[test]
fn theme_migrates_from_dark_theme_and_rejects_junk() {
    assert_eq!(
        parse("[general]\ntheme = \"dark\"\n").general.theme,
        Theme::Dark
    );
    assert_eq!(
        parse("[general]\ntheme = \"neon\"\n").general.theme,
        Theme::Auto
    );
    let cfg = parse("[general]\ndark_theme = true\n");
    assert_eq!(cfg.general.theme, Theme::Dark);
    assert!(!cfg.general.extra.contains_key("dark_theme"));
    assert_eq!(
        parse("[general]\ndark_theme = false\n").general.theme,
        Theme::Light
    );
    // An explicit theme beats the old flag.
    let cfg = parse("[general]\ntheme = \"light\"\ndark_theme = true\n");
    assert_eq!(cfg.general.theme, Theme::Light);
}

#[test]
fn grid_layout_is_read_from_general_when_session_lacks_it() {
    assert_eq!(
        parse("[general]\ngrid_layout = \"3x3\"\n")
            .session
            .grid_layout,
        "3x3"
    );
    let cfg = parse("[general]\ngrid_layout = \"3x3\"\n[session]\ngrid_layout = \"4x4\"\n");
    assert_eq!(cfg.session.grid_layout, "4x4");
}

#[test]
fn timeline_visible_defaults_true_and_loads_false() {
    assert!(parse("").general.timeline_visible);
    assert!(
        !parse("[general]\ntimeline_visible = false\n")
            .general
            .timeline_visible
    );
}

#[test]
fn unquoted_search_datetimes_become_text() {
    let cfg = parse(
        "[session]\nsearch_from_time = 2026-02-01T00:00:00\n\
         events_search_to_time = 2026-02-19\nsnapshots_search_from_time = 5\n",
    );
    assert_eq!(cfg.session.search_from_time, "2026-02-01T00:00:00");
    assert_eq!(cfg.session.events_search_to_time, "2026-02-19");
    assert_eq!(cfg.session.snapshots_search_from_time, "");
    let cfg = parse("[session]\nsearch_to_time = \"2026-02-19T23:59:59\"\n");
    assert_eq!(cfg.session.search_to_time, "2026-02-19T23:59:59");
}

#[test]
fn stale_integer_event_types_are_dropped() {
    let cfg = parse("[session]\nevents_search_event_types = [513, 257]\n");
    assert!(cfg.session.events_search_event_types.is_empty());
    let cfg = parse("[session]\nevents_search_event_types = [\"08\", 513, \"25:hikvision\"]\n");
    assert_eq!(
        cfg.session.events_search_event_types,
        ["08", "25:hikvision"]
    );
    assert!(
        parse("[session]\nevents_search_event_types_match_all = true\n")
            .session
            .events_search_event_types_match_all
    );
}

#[test]
fn setting_overrides_drop_what_no_setting_takes() {
    let cfg = parse(
        "[setting_overrides]\ndemuxer_max_bytes_mib = nan\ndemuxer_max_back_bytes_mib = inf\n\
         cache_seconds_default = 3.0\nother = \"not-a-number\"\nwhole = 2\n",
    );
    assert_eq!(
        cfg.setting_overrides,
        BTreeMap::from([
            ("cache_seconds_default".to_string(), 3.0),
            ("whole".to_string(), 2.0)
        ])
    );
    let cfg = parse("[setting_overrides_choice]\nlive_view_stream_profile = \"low\"\nother = 3\n");
    assert_eq!(
        cfg.setting_overrides_choice,
        BTreeMap::from([("live_view_stream_profile".to_string(), "low".to_string())])
    );
    let cfg = parse("[setting_overrides_bool]\nosd_enabled = true\n");
    assert_eq!(
        cfg.setting_overrides_bool,
        BTreeMap::from([("osd_enabled".to_string(), true)])
    );
}

fn in_profile(state: &str) -> String {
    format!(
        "[general]\ndefault_profile = \"nas\"\n[profiles.nas]\nhost = \"192.168.1.10\"\n{state}"
    )
}

#[test]
fn camera_volume_is_held_to_the_slider_range() {
    let cfg = parse(&in_profile(
        "[profiles.nas.camera_volume]\n1 = 250\n2 = -20\n3 = 40\n4 = \"loud\"\n",
    ));
    assert_eq!(
        cfg.profiles["nas"].state.camera_volume,
        BTreeMap::from([(1, 100), (2, 0), (3, 40)])
    );
}

#[test]
fn malformed_event_history_entries_are_dropped_not_fatal() {
    let cfg = parse(&in_profile(
        "[profiles.nas.legacy_event_type_history]\n\
         \"not-a-number\" = { types = [], checked_until = 0 }\n\
         63 = { types = [[513, 0]], checked_until = 5 }\n\
         64 = [\"not\", \"a\", \"table\"]\n",
    ));
    let history = &cfg.profiles["nas"].state.legacy_event_type_history;
    assert_eq!(history.keys().collect::<Vec<_>>(), [&63]);
    assert_eq!(
        history[&63],
        EventTypeHistory {
            types: vec![(513, 0)],
            checked_until: 5
        }
    );
}

#[test]
fn event_history_is_read_from_its_old_key_and_written_under_the_new_one() {
    let cfg = parse(&in_profile(
        "[profiles.nas.event_type_history]\n\
         63 = { types = [[513, 0]], checked_until = 5 }\n\
         64 = { types = [[257, 0]], checked_until = 6 }\n\
         [profiles.nas.legacy_event_type_history]\n\
         63 = { types = [[771, 0]], checked_until = 9 }\n",
    ));
    let state = &cfg.profiles["nas"].state;
    assert_eq!(
        state.legacy_event_type_history,
        BTreeMap::from([
            (
                63,
                EventTypeHistory {
                    types: vec![(771, 0)],
                    checked_until: 9
                }
            ),
            (
                64,
                EventTypeHistory {
                    types: vec![(257, 0)],
                    checked_until: 6
                }
            ),
        ])
    );
    assert!(!cfg.profiles["nas"].extra.contains_key("event_type_history"));
    let out = table(&cfg.to_toml_string().unwrap());
    let nas = out["profiles"]["nas"].as_table().unwrap();
    assert!(!nas.contains_key("event_type_history"));
    assert_eq!(
        nas["legacy_event_type_history"]["64"]["checked_until"],
        Value::from(6)
    );
}

#[test]
fn an_older_shared_config_goes_to_the_default_profile() {
    let text = r#"
[general]
default_profile = "home"
[session]
layout_cameras = { "2x2" = [1, 2, 0, 0] }
search_camera_ids = [1]
[camera_overrides]
5 = "rtsp://cam"
[event_type_history.63]
types = [[513, 0]]
checked_until = 5
[profiles.home]
host = "a"
[profiles.office]
host = "b"
"#;
    let cfg = parse(text);
    let home = &cfg.profiles["home"].state;
    assert_eq!(
        home.camera_overrides,
        BTreeMap::from([(5, "rtsp://cam".to_string())])
    );
    assert_eq!(home.layout_cameras["2x2"], vec![1, 2, 0, 0]);
    assert_eq!(home.search_camera_ids, vec![1]);
    assert_eq!(home.legacy_event_type_history[&63].checked_until, 5);
    assert!(cfg.profiles["office"].state.is_empty());
    // Consumed: the next save holds them only under the profile.
    let out = table(&cfg.to_toml_string().unwrap());
    assert!(!out.contains_key("camera_overrides"));
    assert!(!out.contains_key("event_type_history"));
    assert!(
        !out["session"]
            .as_table()
            .unwrap()
            .contains_key("layout_cameras")
    );
    assert_eq!(
        out["profiles"]["home"]["camera_overrides"]["5"],
        Value::from("rtsp://cam")
    );
    assert!(
        !out["profiles"]["office"]
            .as_table()
            .unwrap()
            .contains_key("camera_overrides")
    );
}

#[test]
fn a_shared_config_does_not_overwrite_a_profile_that_has_its_own() {
    let text = "[general]\ndefault_profile = \"home\"\n[camera_overrides]\n5 = \"old\"\n\
                [profiles.home]\nhost = \"a\"\n[profiles.home.camera_overrides]\n5 = \"new\"\n";
    let cfg = parse(text);
    assert_eq!(cfg.profiles["home"].state.camera_overrides[&5], "new");
}

// ------------------------------------------------------- search filters

#[test]
fn search_filters_round_trip_per_page_and_validate_the_preset() {
    let mut cfg = AppConfig::default();
    cfg.add_profile("nas", Profile::new("h"));
    let filters = SearchFilters {
        camera_ids: vec![1, 3],
        from_time: "2026-02-01T00:00:00".into(),
        to_time: String::new(),
        preset: Some(TimePreset::Last7d),
    };
    cfg.set_search_filters(SearchPage::Snapshots, "nas", &filters);
    assert_eq!(cfg.search_filters(SearchPage::Snapshots, "nas"), filters);
    assert_eq!(cfg.session.snapshots_search_time_preset, "last7d");
    assert_eq!(
        cfg.profiles["nas"].state.snapshots_search_camera_ids,
        [1, 3]
    );
    // Other pages untouched.
    assert_eq!(
        cfg.search_filters(SearchPage::Recordings, "nas"),
        SearchFilters::default()
    );
    assert_eq!(
        cfg.search_filters(SearchPage::Events, "nas").preset,
        Some(TimePreset::Today)
    );
    // A hand-edited preset nobody knows reads as none.
    cfg.session.search_time_preset = "last7days".into();
    assert_eq!(
        cfg.search_filters(SearchPage::Recordings, "nas").preset,
        None
    );
}

// ------------------------------------------------------------ the disk

#[test]
fn missing_file_loads_as_defaults_and_save_creates_parents() {
    let dir = tempfile::tempdir().unwrap();
    let file = dir.path().join("deep").join("er").join("config.toml");
    assert_eq!(load_from(&file).unwrap(), AppConfig::default());

    let mut cfg = AppConfig::default();
    cfg.add_profile(
        "mynas",
        Profile {
            port: 5000,
            https: false,
            ..Profile::new("192.168.1.100")
        },
    );
    cfg.session.grid_layout = "3x3".into();
    cfg.general.poll_interval_cameras = 15;
    cfg.general.timeline_visible = false;
    save_to(&cfg, &file).unwrap();

    let loaded = load_from(&file).unwrap();
    assert_eq!(loaded, cfg);
    assert_eq!(
        loaded.profiles["mynas"].base_url(),
        "http://192.168.1.100:5000"
    );

    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        assert_eq!(
            fs::metadata(&file).unwrap().permissions().mode() & 0o777,
            0o600
        );
    }
}

#[test]
fn save_is_atomic_and_leaves_no_temp_file() {
    let dir = tempfile::tempdir().unwrap();
    let file = dir.path().join("config.toml");
    let mut cfg = AppConfig::default();
    cfg.general.default_profile = "first".into();
    save_to(&cfg, &file).unwrap();
    cfg.general.default_profile = "second".into();
    save_to(&cfg, &file).unwrap();
    assert_eq!(load_from(&file).unwrap().general.default_profile, "second");

    let names: Vec<String> = fs::read_dir(dir.path())
        .unwrap()
        .map(|e| e.unwrap().file_name().to_string_lossy().into_owned())
        .collect();
    assert_eq!(names, ["config.toml"], "temp file left behind: {names:?}");

    // A save that cannot complete (the target is a directory) leaves the
    // previous file untouched and cleans its temp file up.
    let blocked = dir.path().join("blocked");
    fs::create_dir(&blocked).unwrap();
    fs::write(
        blocked.join("config.toml"),
        "[general]\ndefault_profile = \"kept\"\n",
    )
    .unwrap();
    fs::create_dir(blocked.join("config.toml.tmp-dir")).unwrap();
    // Make the rename fail: the temp name is fixed per pid, so pre-create it
    // as a directory holding something so it is neither openable nor removable.
    let tmp = blocked.join(format!("config.toml.{}.new", std::process::id()));
    fs::create_dir(&tmp).unwrap();
    let err = save_to(&cfg, &blocked.join("config.toml")).unwrap_err();
    assert!(matches!(err, ss_config::ConfigError::Write { .. }), "{err}");
    assert_eq!(
        load_from(&blocked.join("config.toml"))
            .unwrap()
            .general
            .default_profile,
        "kept"
    );
}

#[test]
fn an_unreadable_file_is_moved_aside_and_defaults_used() {
    let dir = tempfile::tempdir().unwrap();
    let file = dir.path().join("config.toml");
    fs::write(&file, "this is = not = toml").unwrap();
    assert!(matches!(
        load_from(&file),
        Err(ss_config::ConfigError::Parse { .. })
    ));

    let loaded = load_or_salvage(&file);
    assert_eq!(loaded.config, AppConfig::default());
    let bad = dir.path().join("config.toml.bad");
    assert_eq!(loaded.salvaged_to.as_deref(), Some(bad.as_path()));
    assert!(!file.exists());
    assert_eq!(fs::read_to_string(bad).unwrap(), "this is = not = toml");

    // A good file is left where it is.
    fs::write(&file, "[general]\ndefault_profile = \"x\"\n").unwrap();
    let loaded = load_or_salvage(&file);
    assert_eq!(loaded.config.general.default_profile, "x");
    assert!(loaded.salvaged_to.is_none());
}

#[test]
fn removing_the_default_profile_moves_the_default() {
    let mut cfg = AppConfig::default();
    cfg.add_profile("b", Profile::new("b"));
    cfg.add_profile("a", Profile::new("a"));
    assert_eq!(cfg.general.default_profile, "b");
    cfg.remove_profile("b");
    assert_eq!(cfg.general.default_profile, "a");
    cfg.remove_profile("a");
    assert_eq!(cfg.general.default_profile, "");
    assert!(cfg.default_profile().is_none());
}
