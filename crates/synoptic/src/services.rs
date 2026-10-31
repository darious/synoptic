//! Background services: a tokio runtime owning the `ss_api::Client`, and the
//! glue that applies connection changes to the [`Store`].
//!
//! UI code calls `services.run(async { … })` and awaits the result on the
//! GLib main loop with `glib::spawn_future_local`.

use std::future::Future;
use std::rc::Rc;
use std::sync::{Arc, RwLock};

use gtk::glib;
use ss_api::{Camera, Client, LoginOptions, SsInfo};

use crate::state::{Change, Connection, Store};

pub struct Services {
    rt: tokio::runtime::Handle,
    _thread: std::thread::JoinHandle<()>,
    client: Arc<RwLock<Option<Client>>>,
}

/// Everything the Login dialog collects.
#[derive(Clone, Debug, Default)]
pub struct ConnectParams {
    pub profile: String,
    pub host: String,
    pub port: u16,
    pub https: bool,
    pub verify_ssl: bool,
    pub username: String,
    pub password: String,
    pub otp_code: Option<String>,
    pub trust_device: bool,
    /// Trusted-device token from a previous login, if any.
    pub device_id: Option<String>,
}

impl ConnectParams {
    pub fn base_url(&self) -> String {
        let scheme = if self.https { "https" } else { "http" };
        format!("{scheme}://{}:{}", self.host, self.port)
    }
}

#[derive(Debug)]
pub struct Connected {
    pub info: SsInfo,
    pub cameras: Vec<Camera>,
    /// New trusted-device token to persist (when `trust_device` was set).
    pub device_id: Option<String>,
    pub home_mode: bool,
}

#[derive(Debug)]
pub enum ConnectError {
    /// Two-factor code needed (or wrong). Re-prompt with `otp_code`.
    OtpRequired(String),
    Other(String),
}

impl std::fmt::Display for ConnectError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            ConnectError::OtpRequired(m) | ConnectError::Other(m) => f.write_str(m),
        }
    }
}

impl Services {
    pub fn new() -> Rc<Services> {
        let rt = tokio::runtime::Builder::new_multi_thread()
            .worker_threads(2)
            .thread_name("synoptic-tokio")
            .enable_all()
            .build()
            .expect("tokio runtime");
        let handle = rt.handle().clone();
        let thread = std::thread::Builder::new()
            .name("synoptic-rt".into())
            .spawn(move || rt.block_on(std::future::pending::<()>()))
            .expect("runtime thread");
        Rc::new(Services {
            rt: handle,
            _thread: thread,
            client: Arc::new(RwLock::new(None)),
        })
    }

    /// Tokio handle for long-running tasks (streams).
    pub fn runtime(&self) -> &tokio::runtime::Handle {
        &self.rt
    }

    /// Run a future on the runtime; await the result from the GLib main loop.
    pub fn run<F, T>(&self, fut: F) -> impl Future<Output = T> + use<F, T>
    where
        F: Future<Output = T> + Send + 'static,
        T: Send + 'static,
    {
        let jh = self.rt.spawn(fut);
        async move { jh.await.expect("background task panicked") }
    }

    /// The logged-in API client (cheap clone), if any.
    pub fn client(&self) -> Option<Client> {
        self.client.read().expect("client lock").clone()
    }

    /// Session id and base URL for `ss_stream`.
    pub fn stream_auth(&self) -> Option<(String, String)> {
        let c = self.client()?;
        Some((c.base_url().to_string(), c.sid()?))
    }

    pub fn connect(
        &self,
        p: ConnectParams,
    ) -> impl Future<Output = Result<Connected, ConnectError>> + use<> {
        let slot = self.client.clone();
        self.run(async move {
            let mut builder = Client::builder(p.base_url()).accept_invalid_certs(!p.verify_ssl);
            if let Some(did) = &p.device_id {
                builder = builder.device_id(did.clone());
            }
            let client = builder
                .build()
                .map_err(|e| ConnectError::Other(e.to_string()))?;
            if let Err(e) = client.discover_apis().await {
                tracing::warn!("API discovery failed, using defaults: {e}");
            }
            let opts = LoginOptions {
                otp_code: p.otp_code.clone(),
                device_id: p.device_id.clone(),
                device_name: Some("Synoptic".into()),
                enable_device_token: p.trust_device,
                remember_credentials: true,
            };
            match client.login(&p.username, &p.password, &opts).await {
                Ok(_) => {}
                Err(ss_api::Error::OtpRequired(e)) => {
                    return Err(ConnectError::OtpRequired(e.message.clone()));
                }
                Err(e) => return Err(ConnectError::Other(e.to_string())),
            }
            let info = client
                .ss_info()
                .await
                .map_err(|e| ConnectError::Other(e.to_string()))?;
            let cameras = client.list_cameras().await.unwrap_or_else(|e| {
                tracing::warn!("camera list: {e}");
                Vec::new()
            });
            let device_id = client.device_id();
            let home_mode = client.home_mode().await.map(|h| h.on).unwrap_or(false);
            *slot.write().expect("client lock") = Some(client);
            Ok(Connected {
                info,
                cameras,
                device_id,
                home_mode,
            })
        })
    }

    pub fn refresh_cameras(&self) -> impl Future<Output = Option<Vec<Camera>>> + use<> {
        let client = self.client();
        self.run(async move {
            let client = client?;
            client.list_cameras().await.ok()
        })
    }

    pub fn set_home_mode(&self, on: bool) -> impl Future<Output = Result<(), String>> + use<> {
        let client = self.client();
        self.run(async move {
            let client = client.ok_or_else(|| "not connected".to_string())?;
            client.set_home_mode(on).await.map_err(|e| e.to_string())
        })
    }

    pub fn logout(&self) -> impl Future<Output = ()> + use<> {
        let client = self.client.write().expect("client lock").take();
        self.run(async move {
            if let Some(c) = client {
                c.logout().await;
            }
        })
    }
}

/// Wire store requests to the services: logout, camera polling.
pub fn install(store: &Rc<Store>, services: &Rc<Services>) {
    let store_w = Rc::downgrade(store);
    let services = services.clone();
    store.subscribe(move |change| {
        let Some(store) = store_w.upgrade() else {
            return;
        };
        match change {
            Change::RequestLogout => {
                let fut = services.logout();
                let store = store.clone();
                glib::spawn_future_local(async move {
                    fut.await;
                    store.update(Change::Connection, |s| {
                        s.connection = None;
                        s.cameras.clear();
                        s.selected_slot = None;
                        s.home_mode = false;
                    });
                    store.emit(&Change::Cameras);
                });
            }
            Change::Connection if store.get().logged_in() => {
                start_camera_polling(&store, &services);
            }
            Change::CameraSettings(id) => {
                tracing::info!(camera = id, "camera settings changed; reloading streams");
                store.emit(&Change::RequestReload);
            }
            Change::HomeMode => {
                let on = store.get().home_mode;
                let fut = services.set_home_mode(on);
                let store = store.clone();
                glib::spawn_future_local(async move {
                    if let Err(e) = fut.await {
                        tracing::warn!("home mode: {e}");
                        store.toast(format!("Home mode failed: {e}"));
                        // revert silently (no second API call)
                        store.state_mut().home_mode = !on;
                        store.emit(&Change::HomeMode);
                    }
                });
            }
            _ => {}
        }
    });
}

/// Apply a successful [`Connected`] result to the store.
pub fn apply_connected(store: &Rc<Store>, p: &ConnectParams, c: Connected) {
    let conn = Connection {
        profile: p.profile.clone(),
        host: p.host.clone(),
        username: p.username.clone(),
        ss_version: c
            .info
            .version
            .as_ref()
            .map(|v| v.to_string())
            .unwrap_or_default(),
        ds_model: c.info.ds_model_name.clone(),
    };
    // layouts and camera order belong to the profile we just logged in to
    let (layouts, active_layout, camera_order) =
        crate::state::profile_layouts(&mut store.config(), &p.profile, &c.cameras);
    store.update(Change::Connection, |s| {
        s.connection = Some(conn);
        s.cameras = c.cameras;
        s.home_mode = c.home_mode;
        s.layouts = layouts;
        s.active_layout = active_layout;
        s.camera_order = camera_order;
        s.selected_slot = None;
    });
    store.emit(&Change::CameraOrder);
    store.emit(&Change::Layout);
    store.emit(&Change::Slots);
    store.emit(&Change::HomeMode);
    store.emit(&Change::Cameras);
    store.toast(format!("Connected to {}", p.profile));
}

fn start_camera_polling(store: &Rc<Store>, services: &Rc<Services>) {
    let interval = store
        .get()
        .connection
        .as_ref()
        .map(|_| store.config().general.poll_interval_cameras.max(5));
    let Some(secs) = interval else { return };
    let store_w = Rc::downgrade(store);
    let services = services.clone();
    glib::timeout_add_seconds_local(secs, move || {
        let Some(store) = store_w.upgrade() else {
            return glib::ControlFlow::Break;
        };
        if !store.get().logged_in() {
            return glib::ControlFlow::Break;
        }
        let fut = services.refresh_cameras();
        let store = store.clone();
        glib::spawn_future_local(async move {
            if let Some(cams) = fut.await {
                let changed = {
                    let s = store.get();
                    s.cameras.len() != cams.len()
                        || s.cameras.iter().zip(&cams).any(|(a, b)| a != b)
                };
                if changed {
                    store.update(Change::Cameras, |s| s.cameras = cams);
                }
            }
        });
        glib::ControlFlow::Continue
    });
}
