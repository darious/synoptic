//! Login dialog (400 w) and the silent auto-login at startup.
//!
//! Flow: collect the form → `Services::connect` → on `OtpRequired` swap the
//! body to a 6-digit code entry and retry with `otp_code`/`trust_device` →
//! on success persist the profile (and the credentials when "Remember" is
//! on), make it the default profile, apply the connection and go to Live
//! View. Errors show inline and never carry the password or session id.

use std::cell::RefCell;
use std::future::Future;
use std::rc::Rc;

use gtk::glib;
use gtk::prelude::*;
use ss_config::{Credentials, KeyringStore, Profile, SecretStore};

use crate::dialogs::shell;
use crate::services::{self, ConnectError, ConnectParams, Connected, Services};
use crate::state::{Change, Page, Store};
use crate::widgets::controls::{divider_h, label};

const NEW_PROFILE: &str = "New profile…";
const FORM: &str = "form";
const OTP: &str = "otp";

/// Parse the Port field.
pub fn parse_port(text: &str) -> Result<u16, String> {
    match text.trim().parse::<u16>() {
        Ok(p) if p > 0 => Ok(p),
        _ => Err("Enter a port between 1 and 65535.".into()),
    }
}

/// Check the form fields; `Ok` carries nothing, the caller already has them.
pub fn validate(profile: &str, host: &str, port: &str, username: &str) -> Result<u16, String> {
    if profile.trim().is_empty() {
        return Err("Enter a profile name.".into());
    }
    if host.trim().is_empty() {
        return Err("Enter the NAS host name or IP address.".into());
    }
    let port = parse_port(port)?;
    if username.trim().is_empty() {
        return Err("Enter a username.".into());
    }
    Ok(port)
}

/// A 6-digit one-time code?
pub fn valid_otp(code: &str) -> bool {
    code.len() == 6 && code.chars().all(|c| c.is_ascii_digit())
}

/// The trusted-device token of `profile`, if it has one.
fn device_id_of(profile: Option<&Profile>) -> Option<String> {
    profile
        .map(|p| p.device_id.trim().to_string())
        .filter(|d| !d.is_empty())
}

// --- keyring (blocking, run on the tokio pool) ------------------------------

fn load_credentials(
    services: &Services,
    profile: String,
) -> impl Future<Output = Option<Credentials>> + use<> {
    services.run(async move {
        let name = profile.clone();
        let r = tokio::task::spawn_blocking(move || KeyringStore.get(&name)).await;
        match r {
            Ok(Ok(c)) => c,
            Ok(Err(e)) => {
                tracing::info!("keyring read for {profile}: {e}");
                None
            }
            Err(e) => {
                tracing::warn!("keyring task: {e}");
                None
            }
        }
    })
}

fn store_credentials(
    services: &Services,
    profile: String,
    credentials: Credentials,
) -> impl Future<Output = ()> + use<> {
    services.run(async move {
        let name = profile.clone();
        let r = tokio::task::spawn_blocking(move || KeyringStore.store(&name, &credentials)).await;
        match r {
            Ok(Ok(())) => {}
            Ok(Err(e)) => tracing::info!("keyring write for {profile}: {e}"),
            Err(e) => tracing::warn!("keyring task: {e}"),
        }
    })
}

fn delete_credentials(services: &Services, profile: String) -> impl Future<Output = ()> + use<> {
    services.run(async move {
        let name = profile.clone();
        let r = tokio::task::spawn_blocking(move || KeyringStore.delete(&name)).await;
        match r {
            Ok(Ok(())) => {}
            Ok(Err(e)) => tracing::info!("keyring delete for {profile}: {e}"),
            Err(e) => tracing::warn!("keyring task: {e}"),
        }
    })
}

// --- persistence after a successful login ----------------------------------

/// Write the profile from `p` (+ the device token) and make it the default.
fn save_profile(store: &Rc<Store>, p: &ConnectParams, device_id: Option<&str>) {
    {
        let mut cfg = store.config();
        let mut profile = cfg
            .profiles
            .get(&p.profile)
            .cloned()
            .unwrap_or_else(|| Profile::new(p.host.clone()));
        profile.host = p.host.clone();
        profile.port = p.port;
        profile.https = p.https;
        profile.verify_ssl = p.verify_ssl;
        profile.username = Some(p.username.clone());
        if let Some(did) = device_id {
            profile.device_id = did.to_string();
        }
        cfg.add_profile(p.profile.clone(), profile);
        cfg.general.default_profile = p.profile.clone();
    }
    store.save_soon();
}

fn finish(
    store: &Rc<Store>,
    services: &Rc<Services>,
    params: ConnectParams,
    connected: Connected,
    remember: bool,
) {
    save_profile(store, &params, connected.device_id.as_deref());
    let fut: std::pin::Pin<Box<dyn Future<Output = ()>>> = if remember {
        Box::pin(store_credentials(
            services,
            params.profile.clone(),
            Credentials {
                username: params.username.clone(),
                password: params.password.clone(),
            },
        ))
    } else {
        // As the reference: an unticked box also forgets a stored password.
        Box::pin(delete_credentials(services, params.profile.clone()))
    };
    glib::spawn_future_local(fut);
    services::apply_connected(store, &params, connected);
    store.update(Change::Page, |s| s.page = Page::Live);
}

// --- startup ---------------------------------------------------------------

/// Try the default profile with its keyring credentials once, silently.
/// Any failure (no credentials, OTP needed, network) leaves the app in the
/// logged-out state without opening a dialog.
pub fn auto_login(store: &Rc<Store>, services: &Rc<Services>) {
    let (name, profile) = {
        let cfg = store.config();
        let name = cfg.general.default_profile.clone();
        match cfg.profiles.get(&name) {
            Some(p) if !name.is_empty() => (name, p.clone()),
            _ => return,
        }
    };
    let creds = load_credentials(services, name.clone());
    let store = store.clone();
    let services = services.clone();
    glib::spawn_future_local(async move {
        let Some(creds) = creds.await else {
            tracing::info!("auto-login: no stored credentials for {name}");
            return;
        };
        let params = ConnectParams {
            profile: name.clone(),
            host: profile.host.clone(),
            port: profile.port,
            https: profile.https,
            verify_ssl: profile.verify_ssl,
            username: creds.username,
            password: creds.password,
            otp_code: None,
            trust_device: false,
            device_id: device_id_of(Some(&profile)),
        };
        match services.connect(params.clone()).await {
            Ok(connected) => {
                if let Some(did) = connected.device_id.as_deref()
                    && did != profile.device_id
                {
                    save_profile(&store, &params, Some(did));
                }
                services::apply_connected(&store, &params, connected);
            }
            Err(e) => tracing::info!("auto-login to {name} failed: {e}"),
        }
    });
}

// --- the dialog --------------------------------------------------------------

struct Ui {
    stack: gtk::Stack,
    names: Vec<String>,
    profile: gtk::DropDown,
    name_field: gtk::Box,
    name_entry: gtk::Entry,
    host: gtk::Entry,
    port: gtk::Entry,
    https: gtk::CheckButton,
    verify: gtk::CheckButton,
    username: gtk::Entry,
    password: gtk::PasswordEntry,
    remember: gtk::CheckButton,
    error: gtk::Label,
    code: gtk::Entry,
    trust: gtk::CheckButton,
    otp_error: gtk::Label,
    connect: gtk::Button,
    cancel: gtk::Button,
    /// Params of the login that asked for a code; retried with `otp_code`.
    pending: RefCell<Option<ConnectParams>>,
}

impl Ui {
    fn in_otp(&self) -> bool {
        self.stack.visible_child_name().as_deref() == Some(OTP)
    }

    fn selected_profile(&self) -> Option<&str> {
        self.names
            .get(self.profile.selected() as usize)
            .map(String::as_str)
    }

    fn profile_name(&self) -> String {
        match self.selected_profile() {
            Some(n) => n.to_string(),
            None => self.name_entry.text().trim().to_string(),
        }
    }

    fn collect(&self, store: &Store) -> Result<ConnectParams, String> {
        let name = self.profile_name();
        let host = self.host.text().trim().to_string();
        let username = self.username.text().trim().to_string();
        let port = validate(&name, &host, &self.port.text(), &username)?;
        let device_id = device_id_of(store.config().profiles.get(&name));
        Ok(ConnectParams {
            profile: name,
            host,
            port,
            https: self.https.is_active(),
            verify_ssl: self.verify.is_active(),
            username,
            password: self.password.text().to_string(),
            otp_code: None,
            trust_device: false,
            device_id,
        })
    }

    fn show_error(&self, msg: &str) {
        let target = if self.in_otp() {
            &self.otp_error
        } else {
            &self.error
        };
        target.set_text(msg);
        target.set_visible(!msg.is_empty());
    }

    fn set_busy(&self, busy: bool) {
        self.connect.set_sensitive(!busy);
        self.cancel.set_sensitive(!busy);
        self.connect.set_label(match (busy, self.in_otp()) {
            (true, _) => "Connecting…",
            (false, true) => "Verify",
            (false, false) => "Connect",
        });
    }

    fn show_otp(&self) {
        self.stack.set_visible_child_name(OTP);
        self.code.set_text("");
        self.otp_error.set_visible(false);
        self.connect.set_label("Verify");
        self.code.grab_focus();
    }

    /// Fill the form from a profile (or blank it for a new one).
    fn prefill(&self, store: &Store, idx: u32) {
        let is_new = idx as usize >= self.names.len();
        self.name_field.set_visible(is_new);
        self.error.set_visible(false);
        if is_new {
            self.name_entry.set_text("");
            self.host.set_text("");
            self.port.set_text(&ss_config::DEFAULT_PORT.to_string());
            self.https.set_active(true);
            self.verify.set_active(false);
            self.username.set_text("");
            self.password.set_text("");
            return;
        }
        let profile = store
            .config()
            .profiles
            .get(&self.names[idx as usize])
            .cloned();
        if let Some(p) = profile {
            self.host.set_text(&p.host);
            self.port.set_text(&p.port.to_string());
            self.https.set_active(p.https);
            self.verify.set_active(p.verify_ssl);
            self.username.set_text(p.username.as_deref().unwrap_or(""));
            self.password.set_text("");
        }
    }
}

/// A labelled form field.
fn field(text: &str, control: &impl IsA<gtk::Widget>) -> gtk::Box {
    let b = gtk::Box::new(gtk::Orientation::Vertical, 4);
    b.append(&label(text, &["field-label"]));
    b.append(control);
    b
}

fn error_label() -> gtk::Label {
    let l = label("", &["error-text"]);
    l.set_wrap(true);
    l.set_visible(false);
    l
}

/// Open the Login dialog over `parent`.
pub fn open(parent: &impl IsA<gtk::Window>, store: &Rc<Store>, services: &Rc<Services>) {
    let dialog = shell::present(parent, 400, crate::APP_ID, "Connect to NAS", None);

    let names: Vec<String> = store.config().profiles.keys().cloned().collect();
    let mut items: Vec<&str> = names.iter().map(String::as_str).collect();
    items.push(NEW_PROFILE);

    // -- form page
    let form = gtk::Box::new(gtk::Orientation::Vertical, 12);
    let profile = gtk::DropDown::from_strings(&items);
    form.append(&field("Profile", &profile));
    let name_entry = gtk::Entry::new();
    name_entry.set_widget_name("login-profile-name");
    name_entry.set_placeholder_text(Some("e.g. home-nas"));
    let name_field = field("Profile name", &name_entry);
    form.append(&name_field);

    let host_port = gtk::Box::new(gtk::Orientation::Horizontal, 8);
    let host = gtk::Entry::new();
    host.set_widget_name("login-host");
    host.set_placeholder_text(Some("192.168.1.100 or nas.local"));
    host.set_hexpand(true);
    let host_field = field("Host", &host);
    host_field.set_hexpand(true);
    host_port.append(&host_field);
    let port = gtk::Entry::new();
    port.set_widget_name("login-port");
    port.set_width_request(100);
    port.set_input_purpose(gtk::InputPurpose::Digits);
    port.set_max_length(5);
    host_port.append(&field("Port", &port));
    form.append(&host_port);

    let https = gtk::CheckButton::with_label("Use HTTPS");
    https.set_widget_name("login-https");
    https.set_active(true);
    let verify = gtk::CheckButton::with_label("Verify SSL certificate");
    let tls_row = gtk::Box::new(gtk::Orientation::Horizontal, 16);
    tls_row.append(&https);
    tls_row.append(&verify);
    form.append(&tls_row);
    form.append(&divider_h());

    let username = gtk::Entry::new();
    username.set_widget_name("login-user");
    form.append(&field("Username", &username));
    let password = gtk::PasswordEntry::new();
    password.set_widget_name("login-password");
    password.set_show_peek_icon(true);
    form.append(&field("Password", &password));
    let remember = gtk::CheckButton::with_label("Remember credentials in system keyring");
    remember.set_active(true);
    form.append(&remember);
    let error = error_label();
    form.append(&error);

    // -- OTP page
    let otp = gtk::Box::new(gtk::Orientation::Vertical, 12);
    let otp_text = label(
        "This account uses two-factor authentication. Enter the 6-digit code from your authenticator app.",
        &["t12", "muted"],
    );
    otp_text.set_wrap(true);
    otp.append(&otp_text);
    let code = gtk::Entry::new();
    code.set_widget_name("login-otp");
    code.add_css_class("otp-entry");
    code.set_max_length(6);
    code.set_input_purpose(gtk::InputPurpose::Digits);
    EntryExt::set_alignment(&code, 0.5);
    code.set_placeholder_text(Some("000000"));
    otp.append(&field("Verification code", &code));
    let trust = gtk::CheckButton::with_label("Trust this device");
    trust.set_active(true);
    otp.append(&trust);
    let otp_error = error_label();
    otp.append(&otp_error);

    let stack = gtk::Stack::new();
    stack.set_transition_type(gtk::StackTransitionType::Crossfade);
    stack.set_vhomogeneous(false);
    stack.add_named(&form, Some(FORM));
    stack.add_named(&otp, Some(OTP));
    dialog.body.append(&stack);

    let cancel = gtk::Button::with_label("Cancel");
    cancel.set_widget_name("login-cancel");
    let connect = gtk::Button::with_label("Connect");
    connect.set_widget_name("login-connect");
    connect.add_css_class("primary");
    dialog.set_footer(&[&cancel, &connect]);

    let ui = Rc::new(Ui {
        stack,
        names,
        profile,
        name_field,
        name_entry,
        host,
        port,
        https,
        verify,
        username,
        password,
        remember,
        error,
        code,
        trust,
        otp_error,
        connect,
        cancel,
        pending: RefCell::new(None),
    });

    // profile selection → prefill + keyring lookup
    {
        let ui_c = ui.clone();
        let store_c = store.clone();
        let services = services.clone();
        let on_select = Rc::new(move |idx: u32| {
            let (ui, store) = (&ui_c, &store_c);
            ui.prefill(store, idx);
            let Some(name) = ui.selected_profile().map(str::to_string) else {
                return;
            };
            let fut = load_credentials(&services, name.clone());
            let ui = ui.clone();
            glib::spawn_future_local(async move {
                if let Some(c) = fut.await
                    && ui.selected_profile() == Some(name.as_str())
                {
                    ui.username.set_text(&c.username);
                    ui.password.set_text(&c.password);
                }
            });
        });
        let initial = {
            let cfg = store.config();
            ui.names
                .iter()
                .position(|n| *n == cfg.general.default_profile)
                .unwrap_or(0) as u32
        };
        ui.profile.set_selected(initial);
        on_select(initial);
        let on_select2 = on_select.clone();
        ui.profile
            .connect_selected_notify(move |d| on_select2(d.selected()));
    }

    {
        let window = dialog.window.clone();
        ui.cancel.connect_clicked(move |_| window.close());
    }

    // Enter in any entry submits.
    for entry in [&ui.host, &ui.port, &ui.username, &ui.name_entry, &ui.code] {
        let connect = ui.connect.clone();
        entry.connect_activate(move |_| connect.emit_clicked());
    }
    {
        let connect = ui.connect.clone();
        ui.password
            .connect_activate(move |_| connect.emit_clicked());
    }

    let window = dialog.window.clone();
    let store = store.clone();
    let services = services.clone();
    let ui2 = ui.clone();
    ui.connect.connect_clicked(move |_| {
        let ui = ui2.clone();
        let in_otp = ui.in_otp();
        let params = if in_otp {
            let Some(mut p) = ui.pending.borrow().clone() else {
                return;
            };
            let code = ui.code.text().trim().to_string();
            if !valid_otp(&code) {
                ui.show_error("Enter the 6-digit code.");
                return;
            }
            p.otp_code = Some(code);
            p.trust_device = ui.trust.is_active();
            p
        } else {
            match ui.collect(&store) {
                Ok(p) => p,
                Err(e) => {
                    ui.show_error(&e);
                    return;
                }
            }
        };
        ui.show_error("");
        ui.set_busy(true);
        let fut = services.connect(params.clone());
        let store = store.clone();
        let services = services.clone();
        let window = window.clone();
        glib::spawn_future_local(async move {
            let result = fut.await;
            ui.set_busy(false);
            match result {
                Ok(connected) => {
                    let remember = ui.remember.is_active();
                    finish(&store, &services, params, connected, remember);
                    window.close();
                }
                Err(ConnectError::OtpRequired(msg)) => {
                    let mut base = params;
                    base.otp_code = None;
                    base.trust_device = false;
                    *ui.pending.borrow_mut() = Some(base);
                    if in_otp {
                        ui.show_error(&msg);
                        ui.code.set_text("");
                        ui.code.grab_focus();
                    } else {
                        ui.show_otp();
                    }
                }
                Err(ConnectError::Other(msg)) => ui.show_error(&msg),
            }
        });
    });
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn port_parsing() {
        assert_eq!(parse_port("5001"), Ok(5001));
        assert_eq!(parse_port(" 443 "), Ok(443));
        assert!(parse_port("0").is_err());
        assert!(parse_port("70000").is_err());
        assert!(parse_port("abc").is_err());
    }

    #[test]
    fn validation_order() {
        assert!(
            validate("", "h", "5001", "u")
                .unwrap_err()
                .contains("profile")
        );
        assert!(
            validate("p", " ", "5001", "u")
                .unwrap_err()
                .contains("host")
        );
        assert!(validate("p", "h", "x", "u").unwrap_err().contains("port"));
        assert!(
            validate("p", "h", "5001", "")
                .unwrap_err()
                .contains("username")
        );
        assert_eq!(validate("p", "h", "5001", "u"), Ok(5001));
    }

    #[test]
    fn otp_shape() {
        assert!(valid_otp("123456"));
        assert!(!valid_otp("12345"));
        assert!(!valid_otp("12345a"));
    }

    #[test]
    fn device_id_blank_is_none() {
        let mut p = Profile::new("h");
        assert_eq!(device_id_of(Some(&p)), None);
        p.device_id = " tok ".into();
        assert_eq!(device_id_of(Some(&p)), Some("tok".into()));
        assert_eq!(device_id_of(None), None);
    }
}
