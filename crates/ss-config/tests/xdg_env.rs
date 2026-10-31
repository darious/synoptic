//! `Paths::from_env` honours `XDG_*`. Alone in its own test binary: the
//! environment is process-wide and `set_var` must not race another
//! thread reading it.

use ss_config::{AppConfig, Paths};

#[test]
fn xdg_config_home_moves_the_config_file() {
    let dir = tempfile::tempdir().unwrap();
    let config_home = dir.path().join("cfg");
    let data_home = dir.path().join("data");
    // SAFETY: the only test in this binary, so no other thread reads the
    // environment while it is written.
    unsafe {
        std::env::set_var("XDG_CONFIG_HOME", &config_home);
        std::env::set_var("XDG_DATA_HOME", &data_home);
        std::env::set_var("XDG_STATE_HOME", dir.path().join("state"));
        std::env::set_var("XDG_CACHE_HOME", dir.path().join("cache"));
    }
    let paths = Paths::from_env().unwrap();
    assert_eq!(paths.config_dir, config_home.join("surveillance-station"));
    assert_eq!(
        paths.config_file(),
        config_home.join("surveillance-station").join("config.toml")
    );
    assert_eq!(paths.data_dir, data_home.join("surveillance-station"));
    assert_eq!(
        paths.state_dir,
        dir.path().join("state").join("surveillance-station")
    );
    assert_eq!(
        paths.cache_dir,
        dir.path().join("cache").join("surveillance-station")
    );
    assert_eq!(
        paths.default_snapshot_dir(),
        data_home.join("surveillance-station").join("snapshots")
    );
    // The default snapshot_dir follows the data directory.
    assert_eq!(
        AppConfig::default().general.snapshot_dir,
        paths.default_snapshot_dir().to_string_lossy()
    );

    // And the standard-location load/save use it.
    let mut cfg = AppConfig::default();
    cfg.general.default_profile = "from-env".into();
    ss_config::save(&cfg).unwrap();
    assert!(paths.config_file().is_file());
    assert_eq!(
        ss_config::load().unwrap().config.general.default_profile,
        "from-env"
    );
}
