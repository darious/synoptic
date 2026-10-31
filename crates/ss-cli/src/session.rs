//! Where to connect and as whom: the saved profile plus command-line
//! overrides, the password from `--password-stdin` / `$SS_PASSWORD` /
//! keyring / prompt, and the login itself.

use std::io::{BufRead, IsTerminal};

use anyhow::{Context, Result, anyhow, bail};
use ss_api::{Client, Error as ApiError, LoginOptions, Session};
use ss_config::{AppConfig, Credentials, KeyringStore, SecretError, SecretStore};

use crate::CliExit;
use crate::cli::{Cli, ConnectArgs};

/// Environment variable holding the password.
pub const ENV_PASSWORD: &str = "SS_PASSWORD";

/// A resolved connection target.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Target {
    /// Profile the values came from / will be saved under, if any.
    pub profile: Option<String>,
    pub host: String,
    pub port: u16,
    pub https: bool,
    /// `true` accepts a self-signed certificate.
    pub accept_invalid_certs: bool,
    /// Trusted-device token from an earlier `login --trust-device`.
    pub device_id: Option<String>,
    /// Account name, if known.
    pub username: Option<String>,
}

impl Target {
    /// `https://host:port`.
    pub fn base_url(&self) -> String {
        let scheme = if self.https { "https" } else { "http" };
        format!("{scheme}://{}:{}", self.host, self.port)
    }

    /// Apply `--host/--port/--https/--insecure/--user`.
    fn apply(&mut self, args: &ConnectArgs) {
        if let Some(h) = &args.host {
            self.host = h.clone();
        }
        if let Some(https) = args.https() {
            self.https = https;
        }
        if let Some(p) = args.port {
            self.port = p;
        }
        if args.insecure {
            self.accept_invalid_certs = true;
        }
        if let Some(u) = &args.user {
            self.username = Some(u.clone());
        }
    }
}

/// Default DSM port for the scheme.
pub fn default_port(https: bool) -> u16 {
    if https { 5001 } else { 5000 }
}

/// The profile name a command addresses: `--profile`, else the config's default.
pub fn profile_name(cli: &Cli, config: &AppConfig) -> Option<String> {
    cli.profile.clone().or_else(|| {
        let d = &config.general.default_profile;
        (!d.is_empty()).then(|| d.clone())
    })
}

/// Resolve the target for every command but `login`: the profile (if any)
/// overridden by the connection flags. Without a profile `--host` is required.
pub fn resolve_target(cli: &Cli, config: &AppConfig) -> Result<Target> {
    let name = profile_name(cli, config);
    let mut target = match name.as_deref().and_then(|n| config.profiles.get(n)) {
        Some(p) => Target {
            profile: name.clone(),
            host: p.host.clone(),
            port: p.port,
            https: p.https,
            accept_invalid_certs: !p.verify_ssl,
            device_id: (!p.device_id.is_empty()).then(|| p.device_id.clone()),
            username: p.username.clone(),
        },
        None => {
            if let Some(n) = &name
                && cli.connect.host.is_none()
            {
                bail!(
                    "profile {n:?} not found; run `ss login --host … --user … --profile {n}` first"
                );
            }
            if cli.connect.host.is_none() {
                bail!(
                    "no profile configured; run `ss login --host HOST --user USER` or pass --host"
                );
            }
            let https = cli.connect.https().unwrap_or(true);
            Target {
                profile: None,
                host: String::new(),
                port: default_port(https),
                https,
                accept_invalid_certs: false,
                device_id: None,
                username: None,
            }
        }
    };
    // A scheme override without a port moves to that scheme's default port.
    if let (Some(https), None) = (cli.connect.https(), cli.connect.port)
        && target.profile.is_none()
    {
        target.port = default_port(https);
    }
    target.apply(&cli.connect);
    Ok(target)
}

/// Resolve the target for `login`: everything from the flags, the saved
/// profile (same name) only supplying the device token.
pub fn login_target(cli: &Cli, config: &AppConfig) -> Result<Target> {
    let host = cli
        .connect
        .host
        .clone()
        .ok_or_else(|| anyhow!("--host is required"))?;
    let user = cli
        .connect
        .user
        .clone()
        .ok_or_else(|| anyhow!("--user is required"))?;
    let https = cli.connect.https().unwrap_or(true);
    let name = cli.profile.clone().unwrap_or_else(|| host.clone());
    let device_id = config
        .profiles
        .get(&name)
        .filter(|p| !p.device_id.is_empty())
        .map(|p| p.device_id.clone());
    Ok(Target {
        profile: Some(name),
        port: cli.connect.port.unwrap_or(default_port(https)),
        host,
        https,
        accept_invalid_certs: cli.connect.insecure,
        device_id,
        username: Some(user),
    })
}

/// Read the password: `--password-stdin` (first line of stdin), else
/// `$SS_PASSWORD`, else the keyring (when `profile` is set and
/// `use_keyring`), else an interactive prompt on a terminal.
pub async fn obtain_credentials(
    target: &Target,
    password_stdin: bool,
    use_keyring: bool,
) -> Result<Credentials> {
    if password_stdin {
        let mut line = String::new();
        std::io::stdin()
            .lock()
            .read_line(&mut line)
            .context("reading the password from stdin")?;
        let password = line.trim_end_matches(['\r', '\n']).to_owned();
        if password.is_empty() {
            bail!("--password-stdin: no password on standard input");
        }
        let username = target
            .username
            .clone()
            .ok_or_else(|| anyhow!("--user is required with --password-stdin"))?;
        return Ok(Credentials { username, password });
    }
    if let Ok(password) = std::env::var(ENV_PASSWORD)
        && !password.is_empty()
    {
        let username = match &target.username {
            Some(u) => u.clone(),
            None => match keyring_lookup(target, use_keyring).await? {
                Some(c) => c.username,
                None => bail!("{ENV_PASSWORD} is set but no user name is known; pass --user"),
            },
        };
        return Ok(Credentials { username, password });
    }
    if let Some(c) = keyring_lookup(target, use_keyring).await? {
        // `--user` wins over the stored name; the stored password is still used.
        return Ok(Credentials {
            username: target.username.clone().unwrap_or(c.username),
            password: c.password,
        });
    }
    let Some(username) = target.username.clone() else {
        bail!("no credentials: run `ss login --host … --user …` or set {ENV_PASSWORD} and --user");
    };
    if !std::io::stdin().is_terminal() {
        bail!(
            "no password: set {ENV_PASSWORD}, use --password-stdin, or run `ss login` on a terminal"
        );
    }
    let prompt = format!("Password for {username}@{}: ", target.host);
    let password = tokio::task::spawn_blocking(move || rpassword::prompt_password(prompt))
        .await
        .context("password prompt")??;
    if password.is_empty() {
        bail!("empty password");
    }
    Ok(Credentials { username, password })
}

async fn keyring_lookup(target: &Target, use_keyring: bool) -> Result<Option<Credentials>> {
    let Some(profile) = target.profile.clone().filter(|_| use_keyring) else {
        return Ok(None);
    };
    match keyring_get(&profile).await {
        Ok(found) => Ok(found),
        Err(SecretError::Unavailable(why)) => {
            tracing::warn!("system keyring unavailable ({why}); set {ENV_PASSWORD} instead");
            Ok(None)
        }
        Err(e) => Err(e).context("reading the keyring"),
    }
}

/// Keyring read on a blocking thread (the secret-service backend blocks).
pub async fn keyring_get(profile: &str) -> Result<Option<Credentials>, SecretError> {
    let profile = profile.to_owned();
    tokio::task::spawn_blocking(move || KeyringStore.get(&profile))
        .await
        .map_err(|e| SecretError::Backend(e.to_string()))?
}

/// Keyring write on a blocking thread.
pub async fn keyring_store(profile: &str, creds: &Credentials) -> Result<(), SecretError> {
    let profile = profile.to_owned();
    let creds = creds.clone();
    tokio::task::spawn_blocking(move || KeyringStore.store(&profile, &creds))
        .await
        .map_err(|e| SecretError::Backend(e.to_string()))?
}

/// This machine's name, for DSM's trusted-device list.
pub fn device_name() -> String {
    std::fs::read_to_string("/proc/sys/kernel/hostname")
        .ok()
        .map(|s| s.trim().to_owned())
        .filter(|s| !s.is_empty())
        .or_else(|| std::env::var("HOSTNAME").ok())
        .filter(|s| !s.is_empty())
        .unwrap_or_else(|| "ss-cli".to_owned())
}

/// Build the API client for `target` and run `SYNO.API.Info` discovery
/// (a discovery failure is logged, not fatal: every call has a fallback path).
pub async fn client_for(target: &Target) -> Result<Client> {
    let mut builder = Client::builder(target.base_url())
        .accept_invalid_certs(target.accept_invalid_certs)
        .user_agent(format!("ss-cli/{}", env!("CARGO_PKG_VERSION")));
    if let Some(did) = &target.device_id {
        builder = builder.device_id(did.clone());
    }
    let client = builder.build().context("building the API client")?;
    match client.discover_apis().await {
        Ok(n) => tracing::debug!(apis = n, "API discovery done"),
        Err(e) => tracing::warn!("API discovery failed ({e}); using default paths and versions"),
    }
    Ok(client)
}

/// Log in. `remember` keeps the credentials in the client for automatic
/// re-login on session expiry. An OTP prompt from the NAS becomes a
/// [`CliExit`] with code [`CliExit::OTP_REQUIRED`] and a hint.
pub async fn login(
    client: &Client,
    creds: &Credentials,
    otp: Option<&str>,
    trust_device: bool,
    remember: bool,
) -> Result<Session> {
    let options = LoginOptions {
        otp_code: otp.map(str::to_owned),
        device_id: None, // the client's stored token is used
        device_name: Some(device_name()),
        enable_device_token: trust_device,
        remember_credentials: remember,
    };
    match client
        .login(&creds.username, &creds.password, &options)
        .await
    {
        Ok(session) => Ok(session),
        Err(ApiError::OtpRequired(e)) => {
            let hint = if otp.is_some() {
                "the code was rejected; try again with a fresh --otp CODE"
            } else {
                "pass --otp CODE (and --trust-device to skip it next time)"
            };
            Err(CliExit::new(
                CliExit::OTP_REQUIRED,
                format!("two-factor authentication required ({e}): {hint}"),
            )
            .into())
        }
        Err(e) => Err(anyhow::Error::new(e).context(format!(
            "login as {} at {} failed",
            creds.username,
            client.base_url()
        ))),
    }
}

/// Load config, resolve the target, get credentials, log in: what every
/// command but `login` starts with.
pub async fn connect(cli: &Cli) -> Result<Connected> {
    let loaded = ss_config::load().context("loading the configuration")?;
    if let Some(bad) = &loaded.salvaged_to {
        tracing::warn!("config was unreadable; moved to {}", bad.display());
    }
    let target = resolve_target(cli, &loaded.config)?;
    let creds = obtain_credentials(&target, false, true).await?;
    let client = client_for(&target).await?;
    let session = login(&client, &creds, None, false, true).await?;
    tracing::info!(
        host = %target.host,
        user = %creds.username,
        has_did = session.did.is_some(),
        "logged in"
    );
    Ok(Connected { target, client })
}

/// A logged-in client and where it points.
pub struct Connected {
    pub target: Target,
    pub client: Client,
}

impl Connected {
    /// The MixStream connector matching the target's TLS setting.
    pub fn ws_connector(&self) -> ss_stream::TungsteniteConnector {
        if self.target.accept_invalid_certs {
            ss_stream::TungsteniteConnector::insecure()
        } else {
            ss_stream::TungsteniteConnector::new()
        }
    }

    /// The session id for the WebSocket cookie.
    pub fn sid(&self) -> Result<String> {
        self.client.sid().ok_or_else(|| anyhow!("not logged in"))
    }

    /// Log out, ignoring failures.
    pub async fn logout(self) {
        self.client.logout().await;
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use clap::Parser;
    use ss_config::Profile;

    fn cli(args: &[&str]) -> Cli {
        Cli::try_parse_from(std::iter::once("ss").chain(args.iter().copied())).unwrap()
    }

    fn config_with(name: &str, profile: Profile, default: bool) -> AppConfig {
        let mut c = AppConfig::default();
        c.add_profile(name, profile);
        if !default {
            c.general.default_profile.clear();
        }
        c
    }

    #[test]
    fn host_flags_alone_make_a_target() {
        let t = resolve_target(
            &cli(&[
                "cameras",
                "--host",
                "127.0.0.1",
                "--port",
                "8080",
                "--no-https",
                "--user",
                "u",
            ]),
            &AppConfig::default(),
        )
        .unwrap();
        assert_eq!(t.base_url(), "http://127.0.0.1:8080");
        assert_eq!(t.profile, None);
        assert!(!t.accept_invalid_certs);
        assert_eq!(t.username.as_deref(), Some("u"));

        let t = resolve_target(&cli(&["cameras", "--host", "nas"]), &AppConfig::default()).unwrap();
        assert_eq!(t.base_url(), "https://nas:5001");
        let t = resolve_target(
            &cli(&["cameras", "--host", "nas", "--no-https"]),
            &AppConfig::default(),
        )
        .unwrap();
        assert_eq!(t.base_url(), "http://nas:5000");
        let t = resolve_target(
            &cli(&["cameras", "--host", "nas", "--insecure"]),
            &AppConfig::default(),
        )
        .unwrap();
        assert!(t.accept_invalid_certs);
    }

    #[test]
    fn missing_profile_and_host_is_an_error() {
        let err = resolve_target(&cli(&["cameras"]), &AppConfig::default()).unwrap_err();
        assert!(err.to_string().contains("ss login"), "{err}");
        let err = resolve_target(&cli(&["cameras", "--profile", "x"]), &AppConfig::default())
            .unwrap_err();
        assert!(err.to_string().contains("\"x\" not found"), "{err}");
    }

    #[test]
    fn profile_values_are_overridden_by_flags() {
        let mut p = Profile::new("nas.local");
        p.port = 5001;
        p.verify_ssl = false;
        p.device_id = "tok".into();
        p.username = Some("alice".into());
        let config = config_with("home", p, true);

        let t = resolve_target(&cli(&["cameras"]), &config).unwrap();
        assert_eq!(t.profile.as_deref(), Some("home"));
        assert_eq!(t.base_url(), "https://nas.local:5001");
        assert!(t.accept_invalid_certs, "verify_ssl=false accepts any cert");
        assert_eq!(t.device_id.as_deref(), Some("tok"));
        assert_eq!(t.username.as_deref(), Some("alice"));

        let t = resolve_target(
            &cli(&[
                "cameras",
                "--host",
                "10.0.0.9",
                "--no-https",
                "--user",
                "bob",
            ]),
            &config,
        )
        .unwrap();
        // A profile's port is kept unless --port is given.
        assert_eq!(t.base_url(), "http://10.0.0.9:5001");
        assert_eq!(t.username.as_deref(), Some("bob"));
        assert_eq!(t.profile.as_deref(), Some("home"));

        // --profile selects a non-default one.
        let mut config = config;
        let mut other = Profile::new("other.local");
        other.verify_ssl = true;
        config.add_profile("work", other);
        let t = resolve_target(&cli(&["cameras", "--profile", "work"]), &config).unwrap();
        assert_eq!(t.host, "other.local");
        assert!(!t.accept_invalid_certs);
    }

    #[test]
    fn login_target_comes_from_flags() {
        assert!(login_target(&cli(&["login", "--user", "u"]), &AppConfig::default()).is_err());
        assert!(login_target(&cli(&["login", "--host", "h"]), &AppConfig::default()).is_err());
        let t = login_target(
            &cli(&["login", "--host", "h", "--user", "u", "--insecure"]),
            &AppConfig::default(),
        )
        .unwrap();
        assert_eq!(t.profile.as_deref(), Some("h"));
        assert_eq!(t.base_url(), "https://h:5001");
        assert!(t.accept_invalid_certs);
        assert_eq!(t.device_id, None);

        let mut p = Profile::new("h");
        p.device_id = "trusted".into();
        let config = config_with("home", p, true);
        let t = login_target(
            &cli(&[
                "login",
                "--host",
                "h",
                "--user",
                "u",
                "--profile",
                "home",
                "--no-https",
            ]),
            &config,
        )
        .unwrap();
        assert_eq!(t.base_url(), "http://h:5000");
        assert_eq!(t.device_id.as_deref(), Some("trusted"));
    }

    #[test]
    fn device_name_is_never_empty() {
        assert!(!device_name().is_empty());
    }
}
