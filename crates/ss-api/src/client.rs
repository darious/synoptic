//! The HTTP client: base URL, TLS options, `SYNO.API.Info` discovery, the
//! generic request/download primitives and the automatic re-login.

use std::collections::HashMap;
use std::fmt;
use std::sync::{Arc, PoisonError, RwLock, RwLockReadGuard, RwLockWriteGuard};
use std::time::Duration;

use bytes::Bytes;
use reqwest::header::CONTENT_TYPE;
use serde_json::Value;
use tokio::io::{AsyncWrite, AsyncWriteExt};
use url::Url;

use crate::auth::{Credentials, LoginOptions};
use crate::de;
use crate::error::{ApiError, Error, ErrorNamespace, Result};
use crate::models::{ApiInfo, Session};

/// Per-request timeout unless overridden with [`ApiRequest::timeout`].
pub const DEFAULT_TIMEOUT: Duration = Duration::from_secs(30);
/// `User-Agent` unless overridden with [`ClientBuilder::user_agent`].
pub const DEFAULT_USER_AGENT: &str = concat!("ss-api/", env!("CARGO_PKG_VERSION"));

const INFO_API: &str = "SYNO.API.Info";
const ENTRY_CGI: &str = "entry.cgi";
const QUERY_CGI: &str = "query.cgi";
/// How much of a download body is inspected for an error page before any
/// of it is written out.
const VALIDATE_HEAD_BYTES: usize = 1024;

/// Builds a [`Client`].
#[derive(Clone, Debug)]
pub struct ClientBuilder {
    base_url: String,
    accept_invalid_certs: bool,
    timeout: Duration,
    user_agent: String,
    device_id: Option<String>,
}

impl ClientBuilder {
    fn new(base_url: &str) -> Self {
        Self {
            base_url: base_url.to_owned(),
            accept_invalid_certs: false,
            timeout: DEFAULT_TIMEOUT,
            user_agent: DEFAULT_USER_AGENT.to_owned(),
            device_id: None,
        }
    }

    /// Accept self-signed / otherwise invalid TLS certificates (the common
    /// case for a NAS on a LAN). Default `false`.
    pub fn accept_invalid_certs(mut self, yes: bool) -> Self {
        self.accept_invalid_certs = yes;
        self
    }

    /// Default timeout for every request. Default 30 s.
    pub fn timeout(mut self, timeout: Duration) -> Self {
        self.timeout = timeout;
        self
    }

    /// `User-Agent` header.
    pub fn user_agent(mut self, ua: impl Into<String>) -> Self {
        self.user_agent = ua.into();
        self
    }

    /// Seed the trusted-device token from an earlier run, so the automatic
    /// re-login can present it. Without it a re-login would be answered by
    /// an OTP prompt no background retry can satisfy.
    pub fn device_id(mut self, did: impl Into<String>) -> Self {
        self.device_id = Some(did.into());
        self
    }

    /// Build the client.
    pub fn build(self) -> Result<Client> {
        let url = Url::parse(&self.base_url).map_err(|e| Error::InvalidUrl(e.to_string()))?;
        if !matches!(url.scheme(), "http" | "https") {
            return Err(Error::InvalidUrl("scheme must be http or https".into()));
        }
        if url.host_str().is_none() {
            return Err(Error::InvalidUrl("missing host".into()));
        }
        if url.query().is_some() || url.fragment().is_some() {
            return Err(Error::InvalidUrl(
                "must not carry a query string or fragment".into(),
            ));
        }
        let base = url.as_str().trim_end_matches('/').to_owned();
        let http = reqwest::Client::builder()
            .use_rustls_tls()
            .danger_accept_invalid_certs(self.accept_invalid_certs)
            .timeout(self.timeout)
            .user_agent(self.user_agent)
            .build()?;
        Ok(Client {
            inner: Arc::new(Inner {
                http,
                base,
                url,
                state: RwLock::new(State {
                    session: None,
                    device_id: self.device_id.filter(|d| !d.is_empty()),
                    credentials: None,
                    api_info: HashMap::new(),
                }),
            }),
        })
    }
}

/// A DSM / Surveillance Station Web API client.
///
/// Cheap to clone; clones share the connection pool, the discovered API
/// table and the session, so one login serves every task.
#[derive(Clone)]
pub struct Client {
    inner: Arc<Inner>,
}

struct Inner {
    http: reqwest::Client,
    /// Base URL without a trailing slash, e.g. `https://nas:5001`.
    base: String,
    url: Url,
    state: RwLock<State>,
}

struct State {
    session: Option<Session>,
    device_id: Option<String>,
    credentials: Option<Credentials>,
    api_info: HashMap<String, ApiInfo>,
}

impl fmt::Debug for Client {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        let st = self.state();
        f.debug_struct("Client")
            .field("base_url", &self.inner.base)
            .field("logged_in", &st.session.is_some())
            .field("apis", &st.api_info.len())
            .finish()
    }
}

/// One Web API call: `api`, `method`, `version` and its parameters.
///
/// ```
/// use ss_api::ApiRequest;
/// let req = ApiRequest::new("SYNO.SurveillanceStation.Camera", "List", 9)
///     .param("basic", true)
///     .param("limit", 50);
/// assert_eq!(req.params().len(), 2);
/// ```
#[derive(Clone, Debug)]
pub struct ApiRequest {
    api: String,
    method: String,
    version: u32,
    params: Vec<(String, String)>,
    post: bool,
    timeout: Option<Duration>,
}

impl ApiRequest {
    /// A GET request with no extra parameters.
    pub fn new(api: impl Into<String>, method: impl Into<String>, version: u32) -> Self {
        Self {
            api: api.into(),
            method: method.into(),
            version,
            params: Vec::new(),
            post: false,
            timeout: None,
        }
    }

    /// Add a parameter. Booleans render as `true`/`false`, numbers as is.
    pub fn param(mut self, key: impl Into<String>, value: impl fmt::Display) -> Self {
        self.params.push((key.into(), value.to_string()));
        self
    }

    /// Add a parameter when `value` is `Some`.
    pub fn param_opt(self, key: impl Into<String>, value: Option<impl fmt::Display>) -> Self {
        match value {
            Some(v) => self.param(key, v),
            None => self,
        }
    }

    /// Send the parameters as a form body instead of the query string, for
    /// the ones that must not end up in an access log (passwords, OTP codes).
    pub fn post(mut self, yes: bool) -> Self {
        self.post = yes;
        self
    }

    /// Override the client's default timeout for this call only.
    pub fn timeout(mut self, timeout: Duration) -> Self {
        self.timeout = Some(timeout);
        self
    }

    /// API name, e.g. `SYNO.SurveillanceStation.Camera`.
    pub fn api(&self) -> &str {
        &self.api
    }

    /// Method name, e.g. `List`.
    pub fn method(&self) -> &str {
        &self.method
    }

    /// Requested version (the client may clamp it, see [`Client::api_version`]).
    pub fn version(&self) -> u32 {
        self.version
    }

    /// Extra parameters, in insertion order.
    pub fn params(&self) -> &[(String, String)] {
        &self.params
    }

    fn label(&self) -> String {
        format!("{}.{}", self.api, self.method)
    }
}

impl Client {
    /// Start building a client for `base_url` (`https://host:5001`, `http://host:5000`,
    /// optionally with a reverse-proxy path prefix).
    pub fn builder(base_url: impl AsRef<str>) -> ClientBuilder {
        ClientBuilder::new(base_url.as_ref())
    }

    /// A client with default options (valid certificates required, 30 s timeout).
    pub fn new(base_url: impl AsRef<str>) -> Result<Self> {
        Self::builder(base_url).build()
    }

    /// Base URL without trailing slash, e.g. `https://nas:5001`.
    pub fn base_url(&self) -> &str {
        &self.inner.base
    }

    /// Parsed base URL (scheme, host, port, path prefix).
    pub fn base(&self) -> &Url {
        &self.inner.url
    }

    fn state(&self) -> RwLockReadGuard<'_, State> {
        self.inner
            .state
            .read()
            .unwrap_or_else(PoisonError::into_inner)
    }

    fn state_mut(&self) -> RwLockWriteGuard<'_, State> {
        self.inner
            .state
            .write()
            .unwrap_or_else(PoisonError::into_inner)
    }

    // ----- session state -------------------------------------------------

    /// The current session, if logged in (or restored with [`Client::set_session`]).
    pub fn session(&self) -> Option<Session> {
        self.state().session.clone()
    }

    /// The current `_sid`, if any.
    pub fn sid(&self) -> Option<String> {
        self.state().session.as_ref().map(|s| s.sid.clone())
    }

    /// Install (or clear) a session, e.g. one saved from an earlier run.
    pub fn set_session(&self, session: Option<Session>) {
        let mut st = self.state_mut();
        if let Some(did) = session.as_ref().and_then(|s| s.did.clone()) {
            st.device_id = Some(did);
        }
        st.session = session;
    }

    /// The trusted-device token presented on (re-)login.
    pub fn device_id(&self) -> Option<String> {
        self.state().device_id.clone()
    }

    /// Set or clear the trusted-device token.
    pub fn set_device_id(&self, did: Option<String>) {
        self.state_mut().device_id = did.filter(|d| !d.is_empty());
    }

    pub(crate) fn store_login(&self, session: &Session, credentials: Option<Credentials>) {
        let mut st = self.state_mut();
        if let Some(did) = &session.did {
            st.device_id = Some(did.clone());
        }
        st.session = Some(session.clone());
        st.credentials = credentials;
    }

    pub(crate) fn clear_login(&self) {
        let mut st = self.state_mut();
        st.session = None;
        st.credentials = None;
    }

    /// Whether a password is held for automatic re-login
    /// (see [`LoginOptions::remember_credentials`]).
    pub fn can_relogin(&self) -> bool {
        self.state().credentials.is_some()
    }

    fn credentials(&self) -> Option<Credentials> {
        self.state().credentials.clone()
    }

    // ----- API discovery -------------------------------------------------

    /// Fetch the `SYNO.API.Info` table (`query.cgi`, `query=all`) and keep
    /// it for path lookup and version clamping. Returns how many APIs the
    /// NAS listed.
    pub async fn discover_apis(&self) -> Result<usize> {
        let url = format!("{}/webapi/{QUERY_CGI}", self.inner.base);
        let resp = self
            .inner
            .http
            .get(url)
            .query(&[
                ("api", INFO_API),
                ("version", "1"),
                ("method", "Query"),
                ("query", "all"),
            ])
            .send()
            .await?;
        let status = resp.status();
        if !status.is_success() {
            return Err(Error::HttpStatus(status.as_u16()));
        }
        let body = resp.bytes().await?;
        let data = unwrap_envelope(&body, ErrorNamespace::Common)?;
        let table: HashMap<String, ApiInfo> = serde_json::from_value(data)
            .map_err(|e| Error::InvalidResponse(format!("{INFO_API}.Query: {e}")))?;
        let n = table.len();
        tracing::debug!(apis = n, "discovered APIs");
        self.state_mut().api_info = table;
        Ok(n)
    }

    /// The discovered entry for `api`, if [`Client::discover_apis`] found it.
    pub fn api_info(&self, api: &str) -> Option<ApiInfo> {
        self.state().api_info.get(api).cloned()
    }

    /// Whether discovery found `api` on this NAS.
    pub fn has_api(&self, api: &str) -> bool {
        self.state().api_info.contains_key(api)
    }

    /// A copy of the whole discovered table.
    pub fn known_apis(&self) -> HashMap<String, ApiInfo> {
        self.state().api_info.clone()
    }

    /// CGI path for `api`: `/webapi/<path>` from discovery, else `/webapi/entry.cgi`.
    pub fn api_path(&self, api: &str) -> String {
        match self.state().api_info.get(api) {
            Some(info) if !info.path.is_empty() => format!("/webapi/{}", info.path),
            _ => format!("/webapi/{ENTRY_CGI}"),
        }
    }

    /// The version to send for `api`: the requested one clamped to the
    /// discovered `maxVersion`; the `maxVersion` itself when nothing is
    /// requested; the requested one (or 1) when the API is unknown.
    pub fn api_version(&self, api: &str, requested: Option<u32>) -> u32 {
        let st = self.state();
        match (st.api_info.get(api), requested.filter(|v| *v > 0)) {
            (Some(info), Some(req)) => req.min(u32::try_from(info.max_version).unwrap_or(1)),
            (Some(info), None) => u32::try_from(info.max_version).unwrap_or(1),
            (None, req) => req.unwrap_or(1),
        }
    }

    /// Full URL for `api`'s CGI (no query string).
    pub fn api_url(&self, api: &str) -> String {
        format!("{}{}", self.inner.base, self.api_path(api))
    }

    /// A URL for a streaming endpoint: `api`'s CGI with `_sid` (when logged
    /// in) and `params` in the query string. Handle with care: it carries
    /// the session id.
    pub fn stream_url(&self, api: &str, params: &[(&str, &str)]) -> Url {
        let mut url = Url::parse(&self.api_url(api)).expect("base URL and CGI path are valid");
        {
            let mut q = url.query_pairs_mut();
            if let Some(sid) = self.sid() {
                q.append_pair("_sid", &sid);
            }
            for (k, v) in params {
                q.append_pair(k, v);
            }
        }
        url
    }

    // ----- generic requests ----------------------------------------------

    fn form(&self, req: &ApiRequest) -> Vec<(String, String)> {
        let version = self.api_version(&req.api, Some(req.version));
        let mut form = Vec::with_capacity(req.params.len() + 4);
        form.push(("api".to_owned(), req.api.clone()));
        form.push(("version".to_owned(), version.to_string()));
        form.push(("method".to_owned(), req.method.clone()));
        if let Some(sid) = self.sid() {
            form.push(("_sid".to_owned(), sid));
        }
        form.extend(req.params.iter().cloned());
        form
    }

    async fn dispatch(&self, req: &ApiRequest) -> Result<reqwest::Response> {
        let url = self.api_url(&req.api);
        let form = self.form(req);
        tracing::debug!(
            api = %req.api,
            method = %req.method,
            version = %form[1].1,
            post = req.post,
            "api request"
        );
        let mut builder = if req.post {
            self.inner.http.post(url).form(&form)
        } else {
            self.inner.http.get(url).query(&form)
        };
        if let Some(t) = req.timeout {
            builder = builder.timeout(t);
        }
        let resp = builder.send().await?;
        let status = resp.status();
        if !status.is_success() {
            return Err(Error::HttpStatus(status.as_u16()));
        }
        Ok(resp)
    }

    /// Send one call without session handling and return the envelope's
    /// `data`. A `success: false` answer becomes the matching [`Error`]
    /// variant; a non-JSON body (a proxy's login page) is a session error.
    pub async fn raw_send(&self, req: &ApiRequest) -> Result<Value> {
        let resp = self.dispatch(req).await?;
        let body = resp.bytes().await?;
        unwrap_envelope(&body, ErrorNamespace::for_api(&req.api))
    }

    async fn relogin(&self, session_error: ApiError) -> Result<()> {
        let Some(creds) = self.credentials() else {
            return Err(Error::SessionExpired(session_error));
        };
        tracing::info!(code = session_error.code, "session error, re-logging in");
        let options = LoginOptions {
            otp_code: None,
            device_id: self.device_id(),
            device_name: creds.device_name.clone(),
            enable_device_token: false,
            remember_credentials: true,
        };
        match self.login(&creds.account, &creds.password, &options).await {
            Ok(_) => Ok(()),
            Err(cause) => Err(Error::ReloginFailed {
                session: session_error,
                cause: Box::new(cause),
            }),
        }
    }

    /// Send one call; on a session error, re-login once with the remembered
    /// credentials (if any) and retry.
    pub async fn send(&self, req: &ApiRequest) -> Result<Value> {
        match self.raw_send(req).await {
            Err(Error::SessionExpired(e)) if self.can_relogin() => {
                self.relogin(e).await?;
                self.raw_send(req).await
            }
            other => other,
        }
    }

    /// Convenience for [`Client::send`] with string parameters.
    pub async fn request(
        &self,
        api: &str,
        method: &str,
        version: u32,
        params: &[(&str, &str)],
    ) -> Result<Value> {
        let mut req = ApiRequest::new(api, method, version);
        for (k, v) in params {
            req = req.param(*k, v);
        }
        self.send(&req).await
    }

    /// [`Client::send`] followed by decoding `data` into `T`.
    pub(crate) async fn send_as<T: serde::de::DeserializeOwned>(
        &self,
        req: &ApiRequest,
    ) -> Result<T> {
        let data = self.send(req).await?;
        decode(data, &req.label())
    }

    // ----- downloads -----------------------------------------------------

    /// Download a binary endpoint into memory. Prefer [`Client::download_to`]
    /// for recordings.
    pub async fn download(&self, req: &ApiRequest) -> Result<Bytes> {
        let mut buf = Vec::new();
        self.download_to(req, &mut buf).await?;
        Ok(Bytes::from(buf))
    }

    /// Stream a binary endpoint (a recording, a snapshot) into `out`.
    ///
    /// Everything DSM can send instead of a file — an HTTP status, a JSON
    /// error, an HTML login page, an empty body — is reported before the
    /// first byte reaches `out`. A session error is retried once after a
    /// re-login, but only when nothing has been written yet. Returns the
    /// number of bytes written.
    pub async fn download_to<W: AsyncWrite + Unpin + ?Sized>(
        &self,
        req: &ApiRequest,
        out: &mut W,
    ) -> Result<u64> {
        let mut written = 0u64;
        match self.download_raw(req, out, &mut written).await {
            Err(Error::SessionExpired(e)) if written == 0 && self.can_relogin() => {
                self.relogin(e).await?;
                self.download_raw(req, out, &mut written).await
            }
            other => other,
        }
    }

    async fn download_raw<W: AsyncWrite + Unpin + ?Sized>(
        &self,
        req: &ApiRequest,
        out: &mut W,
        written: &mut u64,
    ) -> Result<u64> {
        let ns = ErrorNamespace::for_api(&req.api);
        let mut resp = self.dispatch(req).await?;
        let content_type = resp
            .headers()
            .get(CONTENT_TYPE)
            .and_then(|v| v.to_str().ok())
            .unwrap_or("")
            .to_ascii_lowercase();
        if content_type.contains("json") {
            let body = resp.bytes().await?;
            return Err(json_instead_of_file(&body, ns));
        }
        if content_type.contains("text/html") {
            tracing::warn!("download endpoint returned HTML; session may have expired");
            return Err(html_instead_of_file(ns));
        }

        let mut head: Vec<u8> = Vec::new();
        let mut checked = false;
        while let Some(chunk) = resp.chunk().await? {
            if chunk.is_empty() {
                continue;
            }
            if checked {
                out.write_all(&chunk).await?;
                *written += chunk.len() as u64;
                continue;
            }
            head.extend_from_slice(&chunk);
            if head.len() < VALIDATE_HEAD_BYTES {
                continue;
            }
            check_download_head(&head, ns)?;
            checked = true;
            out.write_all(&head).await?;
            *written += head.len() as u64;
            head = Vec::new();
        }
        if !checked {
            // The whole file was smaller than the validation window.
            if head.is_empty() {
                return Err(Error::Api(ApiError::new(
                    100,
                    Some("Server returned an empty response body"),
                    ns,
                )));
            }
            check_download_head(&head, ns)?;
            out.write_all(&head).await?;
            *written += head.len() as u64;
        }
        out.flush().await?;
        Ok(*written)
    }
}

/// Decode `data` into `T`, naming the call in the error.
pub(crate) fn decode<T: serde::de::DeserializeOwned>(data: Value, what: &str) -> Result<T> {
    serde_json::from_value(data).map_err(|e| Error::InvalidResponse(format!("{what}: {e}")))
}

/// Pick the first of `keys` present in `data`, as a list.
pub(crate) fn list_field<'a>(data: &'a Value, keys: &[&str]) -> Option<&'a Value> {
    keys.iter().find_map(|k| data.get(k))
}

/// `data["total"]`, or `fallback` when the server sent none.
pub(crate) fn total_field(data: &Value, fallback: usize) -> i64 {
    data.get("total")
        .map(de::coerce_int)
        .unwrap_or(fallback as i64)
}

/// Decode `{success, data, error:{code,message}}`.
fn unwrap_envelope(body: &[u8], ns: ErrorNamespace) -> Result<Value> {
    let Ok(v) = serde_json::from_slice::<Value>(body) else {
        // A reverse proxy in front of DSM answers a request it will not
        // forward with an HTML login page under a 200. Code 119 is what the
        // download path reports for the same page.
        return Err(Error::from_api_error(ApiError::new(
            119,
            Some("Server returned a non-JSON response (session expired or blocked)"),
            ns,
        )));
    };
    let Some(obj) = v.as_object() else {
        return Err(Error::InvalidResponse(
            "response is not a JSON object".into(),
        ));
    };
    if obj.get("success").and_then(Value::as_bool).unwrap_or(false) {
        return Ok(obj
            .get("data")
            .cloned()
            .unwrap_or_else(|| Value::Object(Default::default())));
    }
    Err(Error::from_api_error(envelope_error(obj.get("error"), ns)))
}

fn envelope_error(error: Option<&Value>, ns: ErrorNamespace) -> ApiError {
    let code = error
        .and_then(|e| e.get("code"))
        .map(de::coerce_int)
        .unwrap_or(100);
    let message = error.and_then(|e| e.get("message")).and_then(Value::as_str);
    ApiError::new(code, message, ns)
}

fn json_instead_of_file(body: &[u8], ns: ErrorNamespace) -> Error {
    let parsed = serde_json::from_slice::<Value>(body).ok();
    let error = parsed.as_ref().and_then(|v| v.get("error"));
    if error.is_some() {
        return Error::from_api_error(envelope_error(error, ns));
    }
    Error::Api(ApiError::new(
        100,
        Some("Server returned JSON instead of a file"),
        ns,
    ))
}

fn html_instead_of_file(ns: ErrorNamespace) -> Error {
    Error::from_api_error(ApiError::new(
        119,
        Some("Server returned an HTML page instead of a file (session expired or access denied)"),
        ns,
    ))
}

/// Recognise an error page or a JSON error in the first bytes of a download
/// that arrived without a telling content type.
fn check_download_head(head: &[u8], ns: ErrorNamespace) -> Result<()> {
    let start = head.len().min(100);
    let probe = String::from_utf8_lossy(&head[..start])
        .trim_start()
        .to_ascii_lowercase();
    if probe.starts_with("<!doctype") || probe.starts_with("<html") {
        return Err(html_instead_of_file(ns));
    }
    if probe.starts_with('{')
        && let Ok(Value::Object(obj)) = serde_json::from_slice::<Value>(head)
        && !obj.get("success").and_then(Value::as_bool).unwrap_or(true)
    {
        return Err(Error::from_api_error(envelope_error(obj.get("error"), ns)));
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn builder_validates_url() {
        assert!(Client::new("https://nas:5001").is_ok());
        assert!(Client::new("http://nas").is_ok());
        assert!(matches!(Client::new("nas:5001"), Err(Error::InvalidUrl(_))));
        assert!(matches!(
            Client::new("ftp://nas:5001"),
            Err(Error::InvalidUrl(_))
        ));
        assert!(matches!(
            Client::new("https://nas:5001/?x=1"),
            Err(Error::InvalidUrl(_))
        ));
        let c = Client::new("https://nas:5001/").unwrap();
        assert_eq!(c.base_url(), "https://nas:5001");
        let c = Client::new("https://nas/prefix/").unwrap();
        assert_eq!(c.api_url("SYNO.X"), "https://nas/prefix/webapi/entry.cgi");
    }

    #[test]
    fn envelope_shapes() {
        let ns = ErrorNamespace::Surveillance;
        let ok = unwrap_envelope(br#"{"success":true,"data":{"a":1}}"#, ns).unwrap();
        assert_eq!(ok["a"], 1);
        let ok = unwrap_envelope(br#"{"success":true}"#, ns).unwrap();
        assert!(ok.is_object());
        let err = unwrap_envelope(br#"{"success":false,"error":{"code":"402"}}"#, ns).unwrap_err();
        assert_eq!(err.code(), Some(402));
        assert_eq!(err.api_error().unwrap().message, "Camera disabled");
        let err = unwrap_envelope(br#"{"success":false}"#, ns).unwrap_err();
        assert_eq!(err.code(), Some(100));
        let err = unwrap_envelope(b"<html>login</html>", ns).unwrap_err();
        assert!(matches!(err, Error::SessionExpired(ref e) if e.code == 119));
        let err = unwrap_envelope(b"[1,2]", ns).unwrap_err();
        assert!(matches!(err, Error::InvalidResponse(_)));
    }

    #[test]
    fn head_sniffing() {
        let ns = ErrorNamespace::Surveillance;
        assert!(check_download_head(b"\x00\x00\x00\x18ftypisom", ns).is_ok());
        assert!(matches!(
            check_download_head(b"  <!DOCTYPE html><html>", ns),
            Err(Error::SessionExpired(_))
        ));
        assert!(matches!(
            check_download_head(b"<html lang=\"en\">", ns),
            Err(Error::SessionExpired(_))
        ));
        let err =
            check_download_head(br#"{"success":false,"error":{"code":400}}"#, ns).unwrap_err();
        assert_eq!(err.code(), Some(400));
        // A truncated JSON head, or one that is not an error, passes.
        assert!(check_download_head(br#"{"success":false,"err"#, ns).is_ok());
        assert!(check_download_head(br#"{"success":true}"#, ns).is_ok());
    }

    #[test]
    fn stream_url_carries_sid_and_params() {
        let c = Client::new("https://nas:5001").unwrap();
        c.set_session(Some(Session {
            sid: "abc".into(),
            did: None,
        }));
        let url = c.stream_url("SYNO.Test", &[("api", "SYNO.Test"), ("method", "Stream")]);
        let s = url.as_str();
        assert!(s.starts_with("https://nas:5001/webapi/entry.cgi?"));
        assert!(
            s.contains("_sid=abc") && s.contains("api=SYNO.Test") && s.contains("method=Stream")
        );
    }

    #[test]
    fn debug_hides_session() {
        let c = Client::new("https://nas:5001").unwrap();
        c.set_session(Some(Session {
            sid: "topsecret".into(),
            did: None,
        }));
        let dbg = format!("{c:?}");
        assert!(!dbg.contains("topsecret"));
        assert!(dbg.contains("logged_in: true"));
    }
}
