//! Error types and the Synology error-code tables.
//!
//! Codes 100–119 are shared by every Synology API. Above that the namespaces
//! diverge: `SYNO.API.Auth` and Surveillance Station both define 400–407 and
//! mean entirely different things by them, so each has its own table and an
//! [`ApiError`] remembers which [`ErrorNamespace`] it was decoded in.
//!
//! Nothing in this module ever puts a session id, a password, an OTP code or
//! a request URL into an error message.

use std::fmt;

/// Codes shared by every Synology Web API.
pub const COMMON_ERRORS: &[(i64, &str)] = &[
    (100, "Unknown error"),
    (101, "Invalid parameters"),
    (102, "API does not exist"),
    (103, "Method does not exist"),
    (104, "This API version is not supported"),
    (105, "Insufficient user privilege"),
    (106, "Connection time out"),
    (107, "Multiple login detected"),
    (119, "SID not found"),
];

/// Surveillance Station's own codes (in addition to [`COMMON_ERRORS`]).
pub const SURVEILLANCE_ERRORS: &[(i64, &str)] = &[
    (400, "Execution failed"),
    (401, "Parameter invalid"),
    (402, "Camera disabled"),
    (407, "CMS closed"),
    (412, "Need to run as admin"),
    (413, "Need to enable home mode first"),
];

/// `SYNO.API.Auth` codes (in addition to [`COMMON_ERRORS`]).
pub const AUTH_ERRORS: &[(i64, &str)] = &[
    (400, "No such account or incorrect password"),
    (401, "Account disabled"),
    (402, "Permission denied"),
    (403, "Two-factor authentication required"),
    (404, "Invalid OTP code"),
    (406, "Two-factor authentication enforced"),
    (407, "Account blocked after too many failed attempts"),
    (408, "Password expired and cannot be changed"),
    (409, "Password expired"),
    (410, "Password must be changed"),
    (411, "Account locked"),
];

/// Codes meaning "the session is gone, log in again".
pub const SESSION_ERROR_CODES: &[i64] = &[105, 106, 107, 119];

/// `SYNO.API.Auth` codes meaning "a two-factor code is needed / was wrong".
pub const OTP_ERROR_CODES: &[i64] = &[403, 404, 406];

/// The API family that produced an error code.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub enum ErrorNamespace {
    /// `SYNO.API.Info` and anything else outside the two tables below.
    Common,
    /// `SYNO.API.Auth.*`
    Auth,
    /// `SYNO.SurveillanceStation.*` (the default for everything else).
    Surveillance,
}

impl ErrorNamespace {
    /// The namespace an API name's error codes belong to.
    pub fn for_api(api: &str) -> Self {
        if api.starts_with(crate::auth::AUTH_API) {
            Self::Auth
        } else {
            Self::Surveillance
        }
    }

    /// The human message for `code` in this namespace, if the tables know it.
    pub fn describe(self, code: i64) -> Option<&'static str> {
        let lookup =
            |table: &[(i64, &'static str)]| table.iter().find(|(c, _)| *c == code).map(|(_, m)| *m);
        lookup(COMMON_ERRORS).or_else(|| match self {
            Self::Common => None,
            Self::Auth => lookup(AUTH_ERRORS),
            Self::Surveillance => lookup(SURVEILLANCE_ERRORS),
        })
    }
}

/// The human message for `code` in `namespace` (see [`ErrorNamespace::describe`]).
pub fn error_message(namespace: ErrorNamespace, code: i64) -> Option<&'static str> {
    namespace.describe(code)
}

/// A `{"success": false, "error": {"code": N}}` answer from the NAS.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct ApiError {
    /// Synology's numeric code.
    pub code: i64,
    /// The server's own message when it sent one, otherwise the table text,
    /// otherwise `Unknown error (N)`.
    pub message: String,
    /// Which table `code` was looked up in.
    pub namespace: ErrorNamespace,
}

impl ApiError {
    /// Build an error for `code`; `server_message` wins over the table.
    pub fn new(code: i64, server_message: Option<&str>, namespace: ErrorNamespace) -> Self {
        let message = server_message
            .filter(|m| !m.is_empty())
            .map(str::to_owned)
            .or_else(|| namespace.describe(code).map(str::to_owned))
            .unwrap_or_else(|| format!("Unknown error ({code})"));
        Self {
            code,
            message,
            namespace,
        }
    }

    /// One of [`SESSION_ERROR_CODES`].
    pub fn is_session_error(&self) -> bool {
        SESSION_ERROR_CODES.contains(&self.code)
    }

    /// One of [`OTP_ERROR_CODES`] *under `SYNO.API.Auth`* — 403 and 404 mean
    /// something else entirely to Surveillance Station.
    pub fn is_otp_error(&self) -> bool {
        self.namespace == ErrorNamespace::Auth && OTP_ERROR_CODES.contains(&self.code)
    }
}

impl fmt::Display for ApiError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "{} (code {})", self.message, self.code)
    }
}

impl std::error::Error for ApiError {}

/// Everything a [`crate::Client`] call can fail with.
#[derive(Debug, thiserror::Error)]
#[non_exhaustive]
pub enum Error {
    /// The NAS answered `success: false` with a code that is neither a
    /// two-factor prompt nor a session error.
    #[error("{0}")]
    Api(ApiError),
    /// `SYNO.API.Auth` code 403, 404 or 406: an OTP code is required, was
    /// wrong, or two-factor is enforced for the account.
    #[error("two-factor authentication required: {0}")]
    OtpRequired(ApiError),
    /// Code 105, 106, 107 or 119, or a non-JSON (login page) body: the
    /// session is gone and a new login is needed.
    #[error("session expired: {0}")]
    SessionExpired(ApiError),
    /// A session error was followed by an automatic re-login that failed.
    #[error("session expired and re-login failed: {cause}")]
    ReloginFailed {
        /// The session error that triggered the re-login.
        session: ApiError,
        /// Why the re-login failed (a rejected password, an OTP prompt, …).
        #[source]
        cause: Box<Error>,
    },
    /// A non-2xx HTTP status. Deliberately carries no URL: every request
    /// but the login puts the session id in the query string. HTTP status
    /// numbers also collide with Synology's own codes, so this is not an
    /// [`ApiError`].
    #[error("server returned HTTP {}", describe_status(*.0))]
    HttpStatus(u16),
    /// The request never produced a response (DNS, TLS, connection reset,
    /// timeout…). The URL is stripped from the wrapped error.
    #[error("HTTP transport error: {0}")]
    Transport(#[source] reqwest::Error),
    /// A 2xx JSON answer that does not have the shape this crate expects.
    #[error("unexpected response: {0}")]
    InvalidResponse(String),
    /// Login reported success without a `sid`.
    #[error("login succeeded but no session id was returned")]
    MissingSid,
    /// The base URL could not be used.
    #[error("invalid base URL: {0}")]
    InvalidUrl(String),
    /// A caller-side argument problem (an empty time range, for instance).
    #[error("invalid argument: {0}")]
    InvalidArgument(String),
    /// Writing a download to its destination failed.
    #[error("I/O error: {0}")]
    Io(#[from] std::io::Error),
}

fn describe_status(status: u16) -> String {
    match reqwest::StatusCode::from_u16(status)
        .ok()
        .and_then(|s| s.canonical_reason())
    {
        Some(reason) => format!("{status} {reason}"),
        None => status.to_string(),
    }
}

impl Error {
    /// Classify a decoded [`ApiError`] into the matching variant.
    pub fn from_api_error(error: ApiError) -> Self {
        if error.is_otp_error() {
            Self::OtpRequired(error)
        } else if error.is_session_error() {
            Self::SessionExpired(error)
        } else {
            Self::Api(error)
        }
    }

    /// The Synology error behind this failure, for any of the API variants.
    pub fn api_error(&self) -> Option<&ApiError> {
        match self {
            Self::Api(e) | Self::OtpRequired(e) | Self::SessionExpired(e) => Some(e),
            Self::ReloginFailed { session, .. } => Some(session),
            _ => None,
        }
    }

    /// Shorthand for `self.api_error().map(|e| e.code)`.
    pub fn code(&self) -> Option<i64> {
        self.api_error().map(|e| e.code)
    }
}

impl From<reqwest::Error> for Error {
    fn from(e: reqwest::Error) -> Self {
        // reqwest's Display quotes the full request URL, `_sid=` included.
        Self::Transport(e.without_url())
    }
}

/// `Result` with this crate's [`Error`].
pub type Result<T, E = Error> = std::result::Result<T, E>;

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn auth_and_surveillance_codes_differ() {
        let auth = ErrorNamespace::Auth;
        let svs = ErrorNamespace::Surveillance;
        assert_eq!(
            ApiError::new(400, None, auth).message,
            "No such account or incorrect password"
        );
        assert_eq!(ApiError::new(400, None, svs).message, "Execution failed");
        assert!(
            ApiError::new(407, None, auth)
                .message
                .starts_with("Account blocked")
        );
        assert_eq!(ApiError::new(407, None, svs).message, "CMS closed");
    }

    #[test]
    fn password_and_lock_codes_are_named() {
        for code in 408..=411 {
            let e = ApiError::new(code, None, ErrorNamespace::Auth);
            assert!(!e.message.contains("Unknown"), "{code}: {}", e.message);
        }
    }

    #[test]
    fn shared_codes_agree_across_namespaces() {
        for (code, text) in COMMON_ERRORS {
            assert_eq!(ErrorNamespace::Auth.describe(*code), Some(*text));
            assert_eq!(ErrorNamespace::Surveillance.describe(*code), Some(*text));
            assert_eq!(ErrorNamespace::Common.describe(*code), Some(*text));
        }
    }

    #[test]
    fn server_message_overrides_table() {
        let e = ApiError::new(400, Some("Camera is offline"), ErrorNamespace::Surveillance);
        assert_eq!(e.message, "Camera is offline");
        let e = ApiError::new(400, Some(""), ErrorNamespace::Surveillance);
        assert_eq!(e.message, "Execution failed");
    }

    #[test]
    fn unknown_code_is_spelled_out() {
        let e = ApiError::new(999, None, ErrorNamespace::Surveillance);
        assert_eq!(e.message, "Unknown error (999)");
        assert_eq!(e.to_string(), "Unknown error (999) (code 999)");
    }

    #[test]
    fn classification() {
        let otp = ApiError::new(403, None, ErrorNamespace::Auth);
        assert!(matches!(Error::from_api_error(otp), Error::OtpRequired(_)));
        let svs_403 = ApiError::new(403, None, ErrorNamespace::Surveillance);
        assert!(matches!(Error::from_api_error(svs_403), Error::Api(_)));
        let gone = ApiError::new(119, None, ErrorNamespace::Surveillance);
        assert!(matches!(
            Error::from_api_error(gone),
            Error::SessionExpired(_)
        ));
        let auth_105 = ApiError::new(105, None, ErrorNamespace::Auth);
        assert!(matches!(
            Error::from_api_error(auth_105),
            Error::SessionExpired(_)
        ));
    }

    #[test]
    fn http_status_display() {
        assert_eq!(
            Error::HttpStatus(500).to_string(),
            "server returned HTTP 500 Internal Server Error"
        );
        assert_eq!(
            Error::HttpStatus(999).to_string(),
            "server returned HTTP 999"
        );
    }
}
