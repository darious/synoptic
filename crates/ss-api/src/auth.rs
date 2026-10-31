//! `SYNO.API.Auth`: login (with OTP and trusted-device tokens) and logout.

use std::fmt;

use serde_json::Value;

use crate::client::{ApiRequest, Client};
use crate::error::{Error, Result};
use crate::models::Session;

/// The authentication API name.
pub const AUTH_API: &str = "SYNO.API.Auth";
/// `Login`/`Logout` version this crate asks for (clamped by discovery).
pub const AUTH_VERSION: u32 = 6;
/// The DSM session name Surveillance Station calls live under.
pub const SESSION_NAME: &str = "SurveillanceStation";

/// Optional login parameters.
#[derive(Clone, Default, PartialEq, Eq)]
pub struct LoginOptions {
    /// The 6-digit two-factor code, when the account has 2FA.
    pub otp_code: Option<String>,
    /// A trusted-device token from an earlier login with
    /// `enable_device_token`; lets the NAS skip the OTP prompt. Falls back
    /// to the client's stored token (see [`Client::set_device_id`]).
    pub device_id: Option<String>,
    /// Name shown in DSM's trusted-device list (the reference uses the
    /// machine's hostname).
    pub device_name: Option<String>,
    /// Ask the NAS to mint a trusted-device token (`did`) for this login,
    /// so later logins can skip the OTP prompt.
    pub enable_device_token: bool,
    /// Keep the account and password in memory so a session error can be
    /// answered with an automatic re-login and retry (what the reference
    /// client always does). Off by default: nothing is retained.
    pub remember_credentials: bool,
}

impl fmt::Debug for LoginOptions {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("LoginOptions")
            .field("otp_code", &self.otp_code.as_ref().map(|_| "<redacted>"))
            .field("device_id", &self.device_id.as_ref().map(|_| "<redacted>"))
            .field("device_name", &self.device_name)
            .field("enable_device_token", &self.enable_device_token)
            .field("remember_credentials", &self.remember_credentials)
            .finish()
    }
}

/// What the automatic re-login needs.
#[derive(Clone)]
pub(crate) struct Credentials {
    pub account: String,
    pub password: String,
    pub device_name: Option<String>,
}

impl fmt::Debug for Credentials {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("Credentials")
            .field("account", &self.account)
            .field("password", &"<redacted>")
            .finish_non_exhaustive()
    }
}

fn non_empty(s: &str) -> bool {
    !s.is_empty()
}

impl Client {
    /// Log in with `SYNO.API.Auth` `Login` v6 (`session=SurveillanceStation`,
    /// `format=sid`), sending the credentials in a POST body so they never
    /// reach an access log. On success the session is stored in the client
    /// and every later request carries its `_sid`.
    ///
    /// Errors: [`Error::OtpRequired`] when a two-factor code is needed or
    /// wrong, [`Error::Api`] with code 400 for a bad password, 401/407/411
    /// for disabled, blocked or locked accounts (see
    /// [`crate::AUTH_ERRORS`]).
    pub async fn login(
        &self,
        account: &str,
        password: &str,
        options: &LoginOptions,
    ) -> Result<Session> {
        let mut req = ApiRequest::new(AUTH_API, "Login", AUTH_VERSION)
            .post(true)
            .param("account", account)
            .param("passwd", password)
            .param("session", SESSION_NAME)
            .param("format", "sid");
        if let Some(otp) = options.otp_code.as_deref().filter(|s| non_empty(s)) {
            req = req.param("otp_code", otp);
        }
        if options.enable_device_token {
            req = req.param("enable_device_token", "yes");
        }
        let device_id = options
            .device_id
            .clone()
            .filter(|s| non_empty(s))
            .or_else(|| self.device_id());
        if let Some(did) = device_id {
            req = req.param("device_id", did);
        }
        if let Some(name) = options.device_name.as_deref().filter(|s| non_empty(s)) {
            req = req.param("device_name", name);
        }

        let data = self.raw_send(&req).await?;
        let sid = data
            .get("sid")
            .and_then(Value::as_str)
            .filter(|s| non_empty(s))
            .ok_or(Error::MissingSid)?
            .to_owned();
        let did = data
            .get("did")
            .and_then(Value::as_str)
            .filter(|s| non_empty(s))
            .map(str::to_owned);
        let session = Session { sid, did };
        let credentials = options.remember_credentials.then(|| Credentials {
            account: account.to_owned(),
            password: password.to_owned(),
            device_name: options.device_name.clone(),
        });
        self.store_login(&session, credentials);
        tracing::info!(account, has_did = session.did.is_some(), "logged in");
        Ok(session)
    }

    /// Log out (`SYNO.API.Auth` `Logout` v6) and forget the session and any
    /// remembered credentials, so nothing still in flight can open a new
    /// session behind the user's back. A failing logout request is logged
    /// and otherwise ignored, as the reference does; the local state is
    /// cleared regardless. The trusted-device token is kept.
    pub async fn logout(&self) {
        if self.sid().is_some() {
            let req =
                ApiRequest::new(AUTH_API, "Logout", AUTH_VERSION).param("session", SESSION_NAME);
            if let Err(e) = self.raw_send(&req).await {
                tracing::debug!(error = %e, "logout request failed");
            }
        }
        self.clear_login();
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn options_debug_redacts_secrets() {
        let o = LoginOptions {
            otp_code: Some("123456".into()),
            device_id: Some("devtoken".into()),
            device_name: Some("laptop".into()),
            ..Default::default()
        };
        let dbg = format!("{o:?}");
        assert!(!dbg.contains("123456") && !dbg.contains("devtoken"));
        assert!(dbg.contains("laptop"));
        let c = Credentials {
            account: "a".into(),
            password: "hunter2".into(),
            device_name: None,
        };
        assert!(!format!("{c:?}").contains("hunter2"));
    }
}
