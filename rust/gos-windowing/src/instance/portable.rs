//! Per-user ownership and acknowledged forwarding for Windows and macOS.
//! The lock file is deliberately never unlinked: replacing its inode would
//! permit a second owner while an old process still held the original lock.
#[cfg(not(target_os = "linux"))]
use super::Claim;
use super::Request;
#[cfg(target_os = "linux")]
pub enum Claim {
    Primary(Listener),
    Forwarded,
}
use crate::{Error, Result, Waker};
use parking_lot::{Condvar, Mutex};
use std::collections::VecDeque;
use std::fs::{File, OpenOptions};
use std::io::{Read, Write};
use std::net::{Ipv4Addr, SocketAddrV4, TcpListener, TcpStream};
use std::sync::{
    Arc,
    atomic::{AtomicBool, Ordering},
};
use std::time::Duration;

const REQUEST_LIMIT: usize = 1024 * 1024;
const QUEUE_LIMIT: usize = 64;
const QUEUE_BYTES: usize = 4 * 1024 * 1024;

#[derive(serde::Serialize, serde::Deserialize)]
struct Endpoint {
    port: u16,
    token: String,
    pid: u32,
}
#[derive(serde::Serialize, serde::Deserialize)]
struct Packet {
    token: String,
    id: String,
    request: Request,
}
#[derive(Default)]
struct Queue {
    requests: Vec<Request>,
    bytes: usize,
    seen: VecDeque<String>,
}
struct Shared {
    queue: Mutex<Queue>,
    ready: Condvar,
    waker: Mutex<Option<Waker>>,
    stopped: AtomicBool,
}

pub struct Listener {
    shared: Arc<Shared>,
    port: u16,
    thread: Mutex<Option<std::thread::JoinHandle<()>>>,
    owner: Mutex<Option<File>>,
    publish: File,
}
fn fail(error: impl std::fmt::Display) -> Error {
    Error::new(error.to_string())
}
fn random() -> Result<String> {
    let mut bytes = [0u8; 32];
    getrandom::fill(&mut bytes).map_err(fail)?;
    Ok(bytes.iter().map(|byte| format!("{byte:02x}")).collect())
}
fn root() -> Result<std::path::PathBuf> {
    #[cfg(target_os = "windows")]
    let base = std::env::var_os("LOCALAPPDATA");
    #[cfg(not(target_os = "windows"))]
    let base = std::env::var_os("HOME");
    let mut path =
        std::path::PathBuf::from(base.ok_or_else(|| fail("user directory unavailable"))?);
    path.push(".gos-instances");
    std::fs::create_dir_all(&path).map_err(fail)?;
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        std::fs::set_permissions(&path, std::fs::Permissions::from_mode(0o700)).map_err(fail)?;
    }
    Ok(path)
}
fn frame(stream: &mut TcpStream, bytes: &[u8]) -> std::io::Result<()> {
    stream.write_all(&(bytes.len() as u32).to_le_bytes())?;
    stream.write_all(bytes)
}
fn receive(stream: &mut TcpStream) -> std::io::Result<Vec<u8>> {
    let mut length = [0; 4];
    stream.read_exact(&mut length)?;
    let length = u32::from_le_bytes(length) as usize;
    if length > REQUEST_LIMIT {
        return Err(std::io::Error::other(
            "instance request exceeds byte budget",
        ));
    }
    let mut bytes = vec![0; length];
    stream.read_exact(&mut bytes)?;
    Ok(bytes)
}
fn connect(port: u16) -> std::io::Result<TcpStream> {
    let stream = TcpStream::connect_timeout(
        &SocketAddrV4::new(Ipv4Addr::LOCALHOST, port).into(),
        Duration::from_millis(500),
    )?;
    stream.set_read_timeout(Some(Duration::from_secs(2)))?;
    stream.set_write_timeout(Some(Duration::from_secs(2)))?;
    Ok(stream)
}

pub fn claim(app_id: &str, request: Request) -> Result<Claim> {
    request.validate()?;
    if app_id.is_empty()
        || app_id.len() > 200
        || !app_id
            .bytes()
            .all(|byte| byte.is_ascii_alphanumeric() || b"._-".contains(&byte))
    {
        return Err(fail(
            "application id must contain 1..200 ASCII letters, digits, dots, hyphens or underscores",
        ));
    }
    let path = root()?.join(format!("{app_id}.lock"));
    let mut options = OpenOptions::new();
    options.read(true).write(true).create(true).truncate(false);
    #[cfg(unix)]
    {
        use std::os::unix::fs::OpenOptionsExt;
        options.mode(0o600);
    }
    let endpoint_path = path.with_extension("endpoint");
    let publish = options.open(path.with_extension("publish")).map_err(fail)?;
    let file = options.open(path).map_err(fail)?;
    let id = random()?;
    // A primary publishes its endpoint and retires while holding the
    // publication lock, so under it the endpoint is either live or its owner
    // has died and released ownership. Two attempts therefore settle a claim.
    publish.lock().map_err(fail)?;
    for _ in 0..2 {
        match file.try_lock() {
            Ok(()) => {
                let claim = primary(file, publish.try_clone().map_err(fail)?, &endpoint_path);
                let _ = publish.unlock();
                return claim;
            }
            Err(std::fs::TryLockError::WouldBlock) => (),
            Err(error) => return Err(fail(error)),
        }
        // Windows locks deny reads through other handles, unlike advisory
        // Unix flock. Discovery must therefore live outside the ownership file.
        let mut bytes = Vec::new();
        if let Ok(endpoint_file) = File::open(&endpoint_path) {
            let _ = endpoint_file.take(4096).read_to_end(&mut bytes);
        }
        if let Ok(endpoint) = serde_json::from_slice::<Endpoint>(&bytes) {
            #[cfg(target_os = "windows")]
            {
                #[link(name = "user32")]
                unsafe extern "system" {
                    fn AllowSetForegroundWindow(process_id: u32) -> i32;
                }
                // SAFETY: grants the existing per-user primary permission to
                // activate after this foreground launch forwards its request.
                unsafe {
                    AllowSetForegroundWindow(endpoint.pid);
                }
            }
            let packet = Packet {
                token: endpoint.token,
                id: id.clone(),
                request: request.clone(),
            };
            let bytes = serde_json::to_vec(&packet).map_err(fail)?;
            if bytes.len() > REQUEST_LIMIT {
                return Err(fail("instance request exceeds byte budget"));
            }
            if let Ok(mut stream) = connect(endpoint.port) {
                let mut ack = [0];
                if frame(&mut stream, &bytes).is_ok()
                    && stream.read_exact(&mut ack).is_ok()
                    && ack == [1]
                {
                    let _ = publish.unlock();
                    return Ok(Claim::Forwarded);
                }
            }
        }
    }
    let _ = publish.unlock();
    Err(fail("the running instance did not accept the request"))
}
fn primary(file: File, publish: File, endpoint_path: &std::path::Path) -> Result<Claim> {
    let socket = TcpListener::bind((Ipv4Addr::LOCALHOST, 0)).map_err(fail)?;
    let port = socket.local_addr().map_err(fail)?.port();
    let token = random()?;
    let endpoint = serde_json::to_vec(&Endpoint {
        port,
        token: token.clone(),
        pid: std::process::id(),
    })
    .map_err(fail)?;
    let mut options = OpenOptions::new();
    options.write(true).create(true).truncate(true);
    #[cfg(unix)]
    {
        use std::os::unix::fs::OpenOptionsExt;
        options.mode(0o600);
    }
    let mut published = options.open(endpoint_path).map_err(fail)?;
    published.write_all(&endpoint).map_err(fail)?;
    published.sync_all().map_err(fail)?;
    drop(published);
    let shared = Arc::new(Shared {
        queue: Mutex::new(Queue::default()),
        ready: Condvar::new(),
        waker: Mutex::new(None),
        stopped: AtomicBool::new(false),
    });
    let state = shared.clone();
    let thread = std::thread::Builder::new()
        .name("instance-forwarding".into())
        .spawn(move || {
            for incoming in socket.incoming() {
                if state.stopped.load(Ordering::Acquire) {
                    break;
                }
                let Ok(mut stream) = incoming else {
                    break;
                };
                let _ = stream.set_read_timeout(Some(Duration::from_secs(2)));
                let _ = stream.set_write_timeout(Some(Duration::from_secs(2)));
                let Ok(bytes) = receive(&mut stream) else {
                    continue;
                };
                let Ok(packet) = serde_json::from_slice::<Packet>(&bytes) else {
                    continue;
                };
                if packet.token != token || packet.id.len() != 64 {
                    continue;
                }
                let mut queue = state.queue.lock();
                let accepted = if queue.seen.contains(&packet.id) {
                    true
                } else if queue.requests.len() < QUEUE_LIMIT
                    && queue.bytes.saturating_add(bytes.len()) <= QUEUE_BYTES
                {
                    queue.bytes += bytes.len();
                    queue.requests.push(packet.request);
                    queue.seen.push_back(packet.id);
                    if queue.seen.len() > 256 {
                        queue.seen.pop_front();
                    }
                    true
                } else {
                    false
                };
                drop(queue);
                if accepted {
                    state.ready.notify_all();
                    if let Some(waker) = &*state.waker.lock() {
                        let _ = waker.wake();
                    }
                    let _ = stream.write_all(&[1]);
                } else {
                    let _ = stream.write_all(&[0]);
                }
            }
            state.ready.notify_all();
        })
        .map_err(fail)?;
    Ok(Claim::Primary(Listener {
        shared,
        port,
        thread: Mutex::new(Some(thread)),
        owner: Mutex::new(Some(file)),
        publish,
    }))
}
impl Listener {
    pub fn take_requests(&self) -> Vec<Request> {
        let mut queue = self.shared.queue.lock();
        queue.bytes = 0;
        std::mem::take(&mut queue.requests)
    }
    /// Waits without polling until a request arrives, closure, or timeout.
    pub fn wait(&self, timeout: Duration) -> bool {
        let mut queue = self.shared.queue.lock();
        if queue.requests.is_empty() && !self.shared.stopped.load(Ordering::Acquire) {
            self.shared.ready.wait_for(&mut queue, timeout);
        }
        !queue.requests.is_empty()
    }
    pub fn wake_with(&self, waker: Waker) {
        *self.shared.waker.lock() = Some(waker.clone());
        if !self.shared.queue.lock().requests.is_empty() {
            let _ = waker.wake();
        }
    }
    pub fn raw_descriptor(&self) -> i64 {
        -1
    }
    pub fn close(&self) {
        let mut thread = self.thread.lock();
        if let Some(thread) = thread.take() {
            // Retiring under the publication lock keeps claimants from reading
            // an endpoint whose owner is between serving and releasing ownership.
            let published = self.publish.lock();
            self.shared.stopped.store(true, Ordering::Release);
            let _ = connect(self.port);
            let _ = thread.join();
            self.shared.queue.lock().requests.clear();
            self.owner.lock().take();
            if published.is_ok() {
                let _ = self.publish.unlock();
            }
        }
    }
}
impl Drop for Listener {
    fn drop(&mut self) {
        self.close();
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::io::BufRead;
    use std::process::{Child, Command, Stdio};

    const READY: &str = "GOS-INSTANCE-OWNER-READY";

    fn spawn_child(id: &str, output: &std::path::Path, mode: &str) -> Child {
        Command::new(std::env::current_exe().unwrap())
            .args(["--exact", "instance::portable::tests::child", "--nocapture"])
            .env("GOS_INSTANCE_TEST_ID", id)
            .env("GOS_INSTANCE_TEST_OUTPUT", output)
            .env("GOS_INSTANCE_TEST_MODE", mode)
            .stdout(Stdio::piped())
            .spawn()
            .unwrap()
    }

    /// Blocks until the owner child reports that it holds the primary claim.
    fn await_owner(owner: &mut Child) {
        let stdout = owner.stdout.take().unwrap();
        let mut lines = std::io::BufReader::new(stdout).lines();
        assert!(lines.any(|line| line.unwrap() == READY));
        std::thread::spawn(move || lines.for_each(drop));
    }

    #[test]
    fn child() {
        let Ok(id) = std::env::var("GOS_INSTANCE_TEST_ID") else {
            return;
        };
        let output = std::env::var("GOS_INSTANCE_TEST_OUTPUT").unwrap();
        let mode = std::env::var("GOS_INSTANCE_TEST_MODE").unwrap();
        let request = Request::Open {
            uris: vec![format!("test:{mode}"), "file:///tmp/a%20b.txt".into()],
        };
        match claim(&id, request).unwrap() {
            Claim::Forwarded => {
                assert!(mode != "owner");
            }
            Claim::Primary(listener) => {
                assert_eq!(mode, "owner");
                println!("{READY}");
                let mut got = Vec::new();
                while got.len() < 12 {
                    listener.wait(Duration::MAX);
                    got.extend(listener.take_requests());
                }
                assert_eq!(got.len(), 12);
                let mut modes = got
                    .iter()
                    .map(|request| match request {
                        Request::Open { uris } => {
                            assert_eq!(uris[1], "file:///tmp/a%20b.txt");
                            uris[0].clone()
                        }
                        _ => panic!("expected open"),
                    })
                    .collect::<Vec<_>>();
                modes.sort();
                modes.dedup();
                assert_eq!(modes.len(), 12);
                listener.close();
                listener.close();
                std::fs::write(output, "done").unwrap();
            }
        }
    }
    #[test]
    fn crashed_owner_releases_lock_and_replaces_stale_endpoint() {
        let id = format!(
            "test.gos.crash.{}.{}",
            std::process::id(),
            random().unwrap()
        );
        let output = std::env::temp_dir().join(&id);
        let mut owner = spawn_child(&id, &output, "owner");
        await_owner(&mut owner);
        let endpoint_path = root().unwrap().join(format!("{id}.endpoint"));
        let old: Endpoint =
            serde_json::from_slice(&std::fs::read(&endpoint_path).unwrap()).unwrap();
        owner.kill().unwrap();
        owner.wait().unwrap();
        let Claim::Primary(listener) = claim(&id, Request::Activate).unwrap() else {
            panic!("crashed owner retained its lock")
        };
        let new: Endpoint =
            serde_json::from_slice(&std::fs::read(&endpoint_path).unwrap()).unwrap();
        assert_ne!(old.token, new.token);
        assert!(matches!(
            claim(&id, Request::Activate).unwrap(),
            Claim::Forwarded
        ));
        assert!(listener.wait(Duration::from_secs(1)));
        assert_eq!(listener.take_requests(), vec![Request::Activate]);
        listener.close();
        std::fs::remove_file(endpoint_path).unwrap();
    }

    #[test]
    fn real_processes_forward_concurrent_launches_and_release_ownership() {
        let id = format!(
            "test.gos.instance.{}.{}",
            std::process::id(),
            random().unwrap()
        );
        let output = std::env::temp_dir().join(&id);
        let mut owner = spawn_child(&id, &output, "owner");
        await_owner(&mut owner);
        let mut peers = (0..12)
            .map(|index| spawn_child(&id, &output, &index.to_string()))
            .collect::<Vec<_>>();
        for peer in &mut peers {
            assert!(peer.wait().unwrap().success());
        }
        assert!(owner.wait().unwrap().success());
        assert_eq!(std::fs::read_to_string(&output).unwrap(), "done");
        let Claim::Primary(listener) = claim(&id, Request::Activate).unwrap() else {
            panic!("ownership leaked")
        };
        let lock = root().unwrap().join(format!("{id}.lock"));
        assert_eq!(std::fs::metadata(&lock).unwrap().len(), 0);
        let endpoint = File::open(lock.with_extension("endpoint")).unwrap();
        assert!(serde_json::from_reader::<_, Endpoint>(endpoint).is_ok());
        assert!(!listener.wait(Duration::from_millis(5)));
        // Installing a waker after forwarding must also wake an already queued request.
        listener.wake_with(Waker {
            wake_type: 0,
            pending: Arc::new(AtomicBool::new(true)),
        });
        assert_eq!(listener.raw_descriptor(), -1);
        assert!(matches!(
            claim(&id, Request::Activate).unwrap(),
            Claim::Forwarded
        ));
        assert!(listener.wait(Duration::from_secs(1)));
        assert_eq!(listener.take_requests(), vec![Request::Activate]);
        listener.close();
        std::fs::remove_file(output).unwrap();
        std::fs::remove_file(lock.with_extension("endpoint")).unwrap();
        // The lock inode deliberately survives listener teardown.
    }
}
