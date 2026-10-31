//! One running instance per application. Linux serves the session-bus name
//! as org.freedesktop.Application. Windows and macOS use a per-user ownership
//! lock and an authenticated loopback endpoint; macOS also delivers native
//! LaunchServices file-open events through Event::OpenRequested.

#[cfg(target_os = "linux")]
use crate::Result;

/// What a later launch asks the running instance to do.
#[derive(Debug, Clone, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
pub enum Request {
    Activate,
    Open { uris: Vec<String> },
}

/// Desktop launch context must accompany the forwarded request on Wayland.
#[derive(Debug, Clone, PartialEq, Eq, Default, serde::Serialize, serde::Deserialize)]
pub struct Activation {
    pub token: String,
    pub startup_id: String,
}
impl Activation {
    pub fn validate(&self) -> crate::Result<()> {
        if self.token.len() > 4096
            || self.startup_id.len() > 4096
            || self.token.contains('\0')
            || self.startup_id.contains('\0')
        {
            return Err(crate::Error::new(
                "activation context exceeds its byte budget or contains NUL",
            ));
        }
        Ok(())
    }
    #[cfg(target_os = "linux")]
    fn environment() -> crate::Result<Self> {
        let context = Self {
            token: std::env::var("XDG_ACTIVATION_TOKEN").unwrap_or_default(),
            startup_id: std::env::var("DESKTOP_STARTUP_ID").unwrap_or_default(),
        };
        context.validate()?;
        Ok(context)
    }
}
#[derive(Debug, Clone, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
pub struct ForwardedRequest {
    pub request: Request,
    pub activation: Activation,
}

impl Request {
    pub(crate) fn validate(&self) -> crate::Result<()> {
        if let Self::Open { uris } = self {
            let bytes = uris.iter().fold(0usize, |sum, uri| {
                sum.saturating_add(uri.len().saturating_mul(8))
                    .saturating_add(64)
            });
            if uris.len() > 4096 || bytes > 1024 * 1024 {
                return Err(crate::Error::new(
                    "instance request exceeds its byte budget",
                ));
            }
        }
        Ok(())
    }
}

/// What a claim found: this process is the instance, or it handed its
/// request to the one already running.
pub enum Claim {
    Primary(Listener),
    Forwarded,
}

fn keeps(b: u8) -> bool {
    b.is_ascii_alphanumeric()
        || matches!(
            b,
            b'-' | b'.'
                | b'_'
                | b'~'
                | b'/'
                | b':'
                | b'@'
                | b'!'
                | b'$'
                | b'&'
                | b'\''
                | b'('
                | b')'
                | b'*'
                | b'+'
                | b','
                | b';'
                | b'='
        )
}

fn path_escape(text: &str) -> String {
    let mut out = String::with_capacity(text.len());
    for b in text.bytes() {
        if keeps(b) {
            out.push(char::from(b));
        } else {
            out.push_str(&format!("%{b:02X}"));
        }
    }
    out
}

pub(crate) fn path_unescape(text: &str) -> String {
    let bytes = text.as_bytes();
    let mut out = Vec::with_capacity(bytes.len());
    let mut i = 0;
    while i < bytes.len() {
        if bytes[i] == b'%'
            && i + 2 < bytes.len()
            && let Ok(hex) = std::str::from_utf8(&bytes[i + 1..i + 3])
            && let Ok(v) = u8::from_str_radix(hex, 16)
        {
            out.push(v);
            i += 3;
            continue;
        }
        out.push(bytes[i]);
        i += 1;
    }
    String::from_utf8_lossy(&out).into_owned()
}

/// The `file://` URI naming the absolute path `path`.
pub fn file_uri(path: &str) -> String {
    let normalized = path.replace('\\', "/");
    if let Some(remote) = normalized.strip_prefix("//") {
        return format!(
            "file://{}",
            remote
                .split('/')
                .map(path_escape)
                .collect::<Vec<_>>()
                .join("/")
        );
    }
    let normalized = if normalized.as_bytes().get(1) == Some(&b':') {
        format!("/{normalized}")
    } else {
        normalized
    };
    let segments: Vec<String> = normalized.split('/').map(path_escape).collect();
    format!("file://{}", segments.join("/"))
}

/// The local path a `file://` URI names, `None` for any other URI.
pub fn uri_path(uri: &str) -> Option<String> {
    let rest = uri.strip_prefix("file://")?;
    // `file://localhost/x` names the same file as `file:///x`.
    let local = rest
        .strip_prefix("localhost")
        .filter(|r| r.starts_with('/'))
        .unwrap_or(rest);
    if !local.starts_with('/') {
        #[cfg(target_os = "windows")]
        if let Some((server, path)) = local.split_once('/')
            && !server.is_empty()
            && !server.contains([':', '@'])
        {
            return Some(native_path(&format!(
                "//{}/{}",
                path_unescape(server),
                path.split('/')
                    .map(path_unescape)
                    .collect::<Vec<_>>()
                    .join("/")
            )));
        }
        return None;
    }
    let decoded = local
        .split('/')
        .map(path_unescape)
        .collect::<Vec<_>>()
        .join("/");
    Some(native_path(&decoded))
}

/// `path`, a decoded `file://` path such as `/C:/a/b`, in the platform's own
/// form: on Windows the drive leads and `\` separates (`C:\a\b`).
pub(crate) fn native_path(path: &str) -> String {
    #[cfg(target_os = "windows")]
    {
        let bytes = path.as_bytes();
        let drive = bytes.first() == Some(&b'/')
            && bytes.get(1).is_some_and(u8::is_ascii_alphabetic)
            && bytes.get(2) == Some(&b':');
        let path = if drive { &path[1..] } else { path };
        path.replace('/', "\\")
    }
    #[cfg(not(target_os = "windows"))]
    path.to_string()
}

#[cfg(target_os = "linux")]
mod linux {
    use std::collections::HashMap;
    use std::os::fd::{AsRawFd, OwnedFd};
    use std::sync::Arc;

    use parking_lot::{Condvar, Mutex};
    use std::sync::atomic::{AtomicBool, Ordering};
    use std::time::Duration;
    use zbus::blocking::Connection;
    use zbus::fdo::{RequestNameFlags, RequestNameReply};
    use zbus::zvariant::{OwnedValue, Value};

    use super::{Activation, Claim, ForwardedRequest, Request};
    use crate::{Error, Result, Waker};

    const INTERFACE: &str = "org.freedesktop.Application";

    /// The running instance's end: requests later launches made.
    pub struct Listener {
        connection: Connection,
        queue: Arc<Mutex<Vec<ForwardedRequest>>>,
        changed: Arc<Condvar>,
        closed: Arc<AtomicBool>,
        // Readable while requests wait, so a goroutine can park on it.
        ready: Arc<OwnedFd>,
        waker: Arc<Mutex<Option<Waker>>>,
        path: String,
        app_id: String,
    }

    struct Application {
        queue: Arc<Mutex<Vec<ForwardedRequest>>>,
        changed: Arc<Condvar>,
        closed: Arc<AtomicBool>,
        ready: Arc<OwnedFd>,
        waker: Arc<Mutex<Option<Waker>>>,
    }

    impl Application {
        fn push(
            &self,
            request: Request,
            platform_data: HashMap<String, OwnedValue>,
        ) -> zbus::fdo::Result<()> {
            let text = |key: &str| -> zbus::fdo::Result<String> {
                let Some(value) = platform_data.get(key) else {
                    return Ok(String::new());
                };
                let value = <&str>::try_from(value).map_err(|_| {
                    zbus::fdo::Error::InvalidArgs(format!("{key} must be a string"))
                })?;
                if value.len() > 4096 || value.contains('\0') {
                    return Err(zbus::fdo::Error::LimitsExceeded(
                        "activation context exceeds its byte budget or contains NUL".into(),
                    ));
                }
                Ok(value.to_string())
            };
            let activation = Activation {
                token: text("activation-token")?,
                startup_id: text("desktop-startup-id")?,
            };
            activation
                .validate()
                .map_err(|error| zbus::fdo::Error::LimitsExceeded(error.to_string()))?;
            request
                .validate()
                .map_err(|error| zbus::fdo::Error::LimitsExceeded(error.to_string()))?;
            let mut queue = self.queue.lock();
            let bytes = |request: &ForwardedRequest| {
                request.activation.token.len()
                    + request.activation.startup_id.len()
                    + match &request.request {
                        Request::Activate => 128,
                        Request::Open { uris } => uris
                            .iter()
                            .fold(128usize, |sum, uri| sum.saturating_add(uri.len() + 64)),
                    }
            };
            let request = ForwardedRequest {
                request,
                activation,
            };
            let added = bytes(&request);
            if self.closed.load(Ordering::Acquire)
                || queue.len() >= 64
                || added > 1024 * 1024
                || queue.iter().map(bytes).sum::<usize>().saturating_add(added) > 4 * 1024 * 1024
            {
                return Err(zbus::fdo::Error::LimitsExceeded(
                    "instance request queue is full or closed".into(),
                ));
            }
            queue.push(request);
            drop(queue);
            self.changed.notify_all();
            let _ = rustix::io::write(&*self.ready, &1u64.to_ne_bytes());
            if let Some(w) = &*self.waker.lock() {
                let _ = w.wake();
            }
            Ok(())
        }
    }

    #[zbus::interface(name = "org.freedesktop.Application")]
    impl Application {
        fn activate(&self, platform_data: HashMap<String, OwnedValue>) -> zbus::fdo::Result<()> {
            self.push(Request::Activate, platform_data)
        }

        fn activate_action(
            &self,
            _name: String,
            _parameter: Vec<OwnedValue>,
            platform_data: HashMap<String, OwnedValue>,
        ) -> zbus::fdo::Result<()> {
            self.push(Request::Activate, platform_data)
        }

        fn open(
            &self,
            uris: Vec<String>,
            platform_data: HashMap<String, OwnedValue>,
        ) -> zbus::fdo::Result<()> {
            self.push(Request::Open { uris }, platform_data)
        }
    }

    /// The object path an application id names: dots become slashes and
    /// dashes underscores.
    pub fn object_path(app_id: &str) -> String {
        let mut path = String::from("/");
        for c in app_id.chars() {
            match c {
                '.' => path.push('/'),
                '-' => path.push('_'),
                other => path.push(other),
            }
        }
        path
    }

    fn bus_error(e: zbus::Error) -> Error {
        Error::new(e.to_string())
    }

    pub fn claim(app_id: &str, request: Request) -> Result<Claim> {
        request.validate()?;
        let activation = Activation::environment()?;
        if app_id.is_empty() {
            return Err(Error::new("claiming an application needs its app_id"));
        }
        let connection =
            crate::bus::session().ok_or_else(|| Error::new("the session bus is unavailable"))?;
        let path = object_path(app_id);
        let queue = Arc::new(Mutex::new(Vec::new()));
        let waker = Arc::new(Mutex::new(None));
        let changed = Arc::new(Condvar::new());
        let closed = Arc::new(AtomicBool::new(false));
        let ready = Arc::new(
            rustix::event::eventfd(
                0,
                rustix::event::EventfdFlags::CLOEXEC | rustix::event::EventfdFlags::NONBLOCK,
            )
            .map_err(|e| Error::new(format!("creating the request signal: {e}")))?,
        );
        let application = || Application {
            queue: queue.clone(),
            changed: changed.clone(),
            closed: closed.clone(),
            ready: ready.clone(),
            waker: waker.clone(),
        };
        // The connection is the process's shared one, which outlives a claim
        // that forwards: such a claim withdraws the object it served.
        let withdraw = |served: bool| {
            if served {
                let _ = connection
                    .object_server()
                    .remove::<Application, _>(path.as_str());
            }
        };
        loop {
            // The object is served before the name is claimed, so a launch
            // racing this one finds it answering. An object already at the
            // path is this process's own primary claim, which the request
            // reaches through the bus like any other launch's.
            let served = connection
                .object_server()
                .at(path.as_str(), application())
                .map_err(bus_error)?;
            // zbus reports a name another peer owns as `NameTaken`, not as a reply.
            let primary = served
                && match connection
                    .request_name_with_flags(app_id, RequestNameFlags::DoNotQueue.into())
                {
                    Ok(reply) => matches!(
                        reply,
                        RequestNameReply::PrimaryOwner | RequestNameReply::AlreadyOwner
                    ),
                    Err(zbus::Error::NameTaken) => false,
                    Err(_) => {
                        withdraw(served);
                        return Err(Error::new(format!(
                            "the session bus refused the name {app_id}"
                        )));
                    }
                };
            if primary {
                return Ok(Claim::Primary(Listener {
                    connection: connection.clone(),
                    queue,
                    changed,
                    closed,
                    ready,
                    waker,
                    path,
                    app_id: app_id.to_string(),
                }));
            }
            let mut platform: HashMap<&str, Value<'_>> = HashMap::new();
            if !activation.token.is_empty() {
                platform.insert("activation-token", Value::from(activation.token.as_str()));
            }
            if !activation.startup_id.is_empty() {
                platform.insert(
                    "desktop-startup-id",
                    Value::from(activation.startup_id.as_str()),
                );
            }
            let answered = match &request {
                Request::Activate => connection.call_method(
                    Some(app_id),
                    path.as_str(),
                    Some(INTERFACE),
                    "Activate",
                    &(platform,),
                ),
                Request::Open { uris } => connection.call_method(
                    Some(app_id),
                    path.as_str(),
                    Some(INTERFACE),
                    "Open",
                    &(uris, platform),
                ),
            };
            withdraw(served);
            match answered {
                Ok(_) => return Ok(Claim::Forwarded),
                Err(zbus::Error::MethodError(name, _, _))
                    if matches!(
                        name.as_str(),
                        "org.freedesktop.DBus.Error.ServiceUnknown"
                            | "org.freedesktop.DBus.Error.NameHasNoOwner"
                    ) =>
                {
                    // The primary exited between ownership and delivery, so the
                    // name is free again: claim it, or reach whoever took it.
                }
                Err(error) => {
                    return Err(Error::new(format!(
                        "the running instance of {app_id} rejected the request: {error}"
                    )));
                }
            }
        }
    }

    impl Listener {
        /// The requests later launches made since the last call, oldest first.
        pub fn take_requests(&self) -> Vec<Request> {
            self.take_forwarded()
                .into_iter()
                .map(|item| item.request)
                .collect()
        }
        /// Drains requests with their per-launch desktop activation context.
        pub fn take_forwarded(&self) -> Vec<ForwardedRequest> {
            let mut drained = [0u8; 8];
            let _ = rustix::io::read(&*self.ready, &mut drained);
            std::mem::take(&mut *self.queue.lock())
        }

        pub fn wait(&self, timeout: Duration) -> bool {
            let mut queue = self.queue.lock();
            if queue.is_empty() && !self.closed.load(Ordering::Acquire) {
                self.changed.wait_for(&mut queue, timeout);
            }
            !queue.is_empty()
        }

        /// Wakes `waker`'s loop whenever a request arrives, so a loop that
        /// serves requests between waits needs no watcher on the descriptor.
        pub fn wake_with(&self, waker: Waker) {
            *self.waker.lock() = Some(waker.clone());
            if !self.queue.lock().is_empty() {
                let _ = waker.wake();
            }
        }

        /// Gives up the claim.
        pub fn close(&self) {
            self.closed.store(true, Ordering::Release);
            self.changed.notify_all();
            let _ = self
                .connection
                .object_server()
                .remove::<Application, _>(self.path.as_str());
            let _ = self.connection.release_name(self.app_id.as_str());
        }

        /// A descriptor that becomes readable when a request arrives.
        pub fn raw_descriptor(&self) -> i64 {
            i64::from(self.ready.as_raw_fd())
        }
    }
    impl Drop for Listener {
        fn drop(&mut self) {
            self.close();
        }
    }
}

#[cfg(target_os = "linux")]
pub use linux::{Listener, object_path};

/// Claims `app_id` for this process, or hands `request` to the running instance.
#[cfg(target_os = "linux")]
pub fn claim(app_id: &str, request: Request) -> Result<Claim> {
    linux::claim(app_id, request)
}

#[cfg(any(not(target_os = "linux"), test))]
#[path = "instance/portable.rs"]
mod portable;
#[cfg(not(target_os = "linux"))]
pub use portable::{Listener, claim};

#[cfg(not(target_os = "linux"))]
impl Listener {
    pub fn take_forwarded(&self) -> Vec<ForwardedRequest> {
        self.take_requests()
            .into_iter()
            .map(|request| ForwardedRequest {
                request,
                activation: Activation::default(),
            })
            .collect()
    }
}

#[cfg(test)]
mod tests {
    use super::{file_uri, uri_path};

    #[test]
    fn activation_context_rejects_oversized_or_nul_tokens() {
        assert!(
            super::Activation {
                token: "x".repeat(4097),
                ..Default::default()
            }
            .validate()
            .is_err()
        );
        assert!(
            super::Activation {
                token: "a\0b".into(),
                ..Default::default()
            }
            .validate()
            .is_err()
        );
    }

    #[test]
    fn a_path_round_trips_through_its_file_uri() {
        // A decoded path answers in the platform's spelling: `\` on Windows.
        let native = |path: &str| {
            Some(if cfg!(target_os = "windows") {
                path.replace('/', "\\")
            } else {
                path.to_string()
            })
        };
        let uri = file_uri("/tmp/a b/ü#1.txt");
        assert_eq!(uri, "file:///tmp/a%20b/%C3%BC%231.txt");
        assert_eq!(uri_path(&uri), native("/tmp/a b/ü#1.txt"));
        assert_eq!(uri_path("file://localhost/etc/hosts"), native("/etc/hosts"));
        assert_eq!(uri_path("https://example.com/x"), None);
        assert_eq!(uri_path("file:///a%ü"), native("/a%ü"));
        assert_eq!(uri_path("file:///a%界"), native("/a%界"));
        assert_eq!(uri_path("file:///a%"), native("/a%"));
        #[cfg(not(target_os = "windows"))]
        assert_eq!(uri_path("file://server/share"), None);
        #[cfg(target_os = "windows")]
        {
            assert_eq!(
                uri_path("file://server/share/x").as_deref(),
                Some(r"\\server\share\x")
            );
            assert_eq!(
                uri_path(&file_uri(r"C:\a b\c.txt")).as_deref(),
                Some(r"C:\a b\c.txt")
            );
            assert_eq!(
                uri_path("file://localhost/C:/a%20b").as_deref(),
                Some(r"C:\a b")
            );
        }
    }

    #[cfg(target_os = "linux")]
    #[test]
    fn an_application_id_names_its_object_path() {
        assert_eq!(
            super::object_path("dev.gossamer.code-editor"),
            "/dev/gossamer/code_editor"
        );
    }
}

#[cfg(all(test, target_os = "linux"))]
mod bus_process_tests {
    use super::*;
    use std::io::BufRead;
    use std::{
        process::{Command, Stdio},
        time::Duration,
    };

    const READY: &str = "GOS-BUS-OWNER-READY";

    #[test]
    fn child() {
        let Ok(id) = std::env::var("GOS_BUS_CHILD_ID") else {
            return;
        };
        let mode = std::env::var("GOS_BUS_CHILD_MODE").unwrap();
        match claim(
            &id,
            Request::Open {
                uris: vec![mode.clone(), "custom:uri".into()],
            },
        )
        .unwrap()
        {
            Claim::Forwarded => assert_ne!(mode, "owner"),
            Claim::Primary(listener) => {
                assert_eq!(mode, "owner");
                println!("{READY}");
                let mut requests = Vec::new();
                while requests.len() < 12 {
                    listener.wait(Duration::MAX);
                    requests.extend(listener.take_forwarded());
                }
                assert_eq!(requests.len(), 12);
                let mut seen = requests
                    .iter()
                    .map(|item| match &item.request {
                        Request::Open { uris } => {
                            assert_eq!(item.activation.token, format!("token-{}", uris[0]));
                            assert_eq!(item.activation.startup_id, format!("startup-{}", uris[0]));
                            assert_eq!(uris[1], "custom:uri");
                            uris[0].clone()
                        }
                        _ => panic!("expected open"),
                    })
                    .collect::<Vec<_>>();
                seen.sort();
                seen.dedup();
                assert_eq!(seen.len(), 12);
                listener.close();
            }
        }
    }
    #[test]
    fn concurrent_launches_and_restart_on_real_session_bus() {
        if std::env::var_os("GOS_BUS_GROUP").is_none() {
            assert!(Command::new("sh").arg(concat!(env!("CARGO_MANIFEST_DIR"), "/../../tools/private-bus.sh"))
                .arg("--").arg(std::env::current_exe().unwrap())
                .args(["--exact", "instance::bus_process_tests::concurrent_launches_and_restart_on_real_session_bus", "--nocapture"])
                .env("GOS_BUS_GROUP", "1").status().unwrap().success());
            return;
        }
        let id = format!("test.gos.instance.p{}", std::process::id());
        let spawn = |mode: &str| {
            Command::new(std::env::current_exe().unwrap())
                .args([
                    "--exact",
                    "instance::bus_process_tests::child",
                    "--nocapture",
                ])
                .env("GOS_BUS_CHILD_ID", &id)
                .env("GOS_BUS_CHILD_MODE", mode)
                .env("XDG_ACTIVATION_TOKEN", format!("token-{mode}"))
                .env("DESKTOP_STARTUP_ID", format!("startup-{mode}"))
                .stdout(Stdio::piped())
                .spawn()
                .unwrap()
        };
        let mut owner = spawn("owner");
        let mut lines = std::io::BufReader::new(owner.stdout.take().unwrap()).lines();
        assert!(lines.any(|line| line.unwrap() == READY));
        std::thread::spawn(move || lines.for_each(drop));
        let mut peers = (0..12)
            .map(|index| spawn(&index.to_string()))
            .collect::<Vec<_>>();
        for peer in &mut peers {
            assert!(peer.wait().unwrap().success());
        }
        assert!(owner.wait().unwrap().success());
        let Claim::Primary(listener) = claim(&id, Request::Activate).unwrap() else {
            panic!("claim leaked")
        };
        assert!(matches!(
            claim(&id, Request::Activate).unwrap(),
            Claim::Forwarded
        ));
        assert!(listener.wait(Duration::from_secs(1)));
        assert_eq!(listener.take_requests(), vec![Request::Activate]);
        listener.close();
    }
}
