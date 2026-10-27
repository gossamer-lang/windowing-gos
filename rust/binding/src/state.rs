//! What the binding keeps between calls: the session, and the handles the
//! Gossamer side holds as ids.

use std::collections::HashMap;
use std::sync::atomic::{AtomicI64, Ordering};

use gos_windowing::cursor::Cursor;
use gos_windowing::instance::Listener;
use gos_windowing::surface::GlContext;
use gos_windowing::tray::Tray;
use gos_windowing::{App, Waker};
use gossamer_binding::GosError;
use parking_lot::Mutex;

static SESSION: Mutex<Option<App>> = Mutex::new(None);
static WAKER: Mutex<Option<Waker>> = Mutex::new(None);
static NEXT_HANDLE: AtomicI64 = AtomicI64::new(1);

struct Handles<T>(Mutex<Option<HashMap<i64, T>>>);

impl<T> Handles<T> {
    const fn new() -> Self {
        Handles(Mutex::new(None))
    }

    fn keep(&self, value: T) -> i64 {
        let id = NEXT_HANDLE.fetch_add(1, Ordering::Relaxed);
        self.0
            .lock()
            .get_or_insert_with(HashMap::new)
            .insert(id, value);
        id
    }

    fn take(&self, id: i64, what: &str) -> Result<T, GosError> {
        self.0
            .lock()
            .as_mut()
            .and_then(|m| m.remove(&id))
            .ok_or_else(|| GosError::from(format!("no {what} {id}")))
    }

    fn with<R>(
        &self,
        id: i64,
        what: &str,
        f: impl FnOnce(&T) -> Result<R, GosError>,
    ) -> Result<R, GosError> {
        let guard = self.0.lock();
        let value = guard
            .as_ref()
            .and_then(|m| m.get(&id))
            .ok_or_else(|| GosError::from(format!("no {what} {id}")))?;
        f(value)
    }
}

struct Sendable<T>(T);
// SAFETY: these handles are SDL objects whose every use checks it runs on
// the main thread; the maps only store them.
unsafe impl<T> Send for Sendable<T> {}

static CURSORS: Handles<Sendable<Cursor>> = Handles::new();
static TRAYS: Handles<Sendable<Tray>> = Handles::new();
static GL: Handles<Sendable<GlContext>> = Handles::new();
static LISTENERS: Handles<Listener> = Handles::new();

pub(crate) fn install(app: App) {
    *WAKER.lock() = Some(app.waker());
    *SESSION.lock() = Some(app);
}

pub(crate) fn finalize() {
    WAKER.lock().take();
    let app = SESSION.lock().take();
    if let Some(app) = app {
        app.finalize();
    }
    CURSORS.0.lock().take();
    TRAYS.0.lock().take();
    GL.0.lock().take();
}

/// Runs `f` with the session's `App`. The lock is released first, so a
/// blocking wait does not hold it.
pub fn with_app<R>(f: impl FnOnce(&App) -> Result<R, GosError>) -> Result<R, GosError> {
    let app = SESSION.lock().clone();
    match app {
        Some(app) => f(&app),
        None => Err(GosError::from(
            "windowing is not running; call windowing::init first",
        )),
    }
}

pub(crate) fn waker() -> Option<Waker> {
    WAKER.lock().clone()
}

pub(crate) fn keep_cursor(c: Cursor) -> i64 {
    CURSORS.keep(Sendable(c))
}

pub(crate) fn cursor(id: i64) -> Result<Cursor, GosError> {
    CURSORS.with(id, "cursor", |c| Ok(c.0))
}

pub(crate) fn take_cursor(id: i64) -> Result<Cursor, GosError> {
    CURSORS.take(id, "cursor").map(|c| c.0)
}

pub(crate) fn keep_tray(t: Tray) -> i64 {
    TRAYS.keep(Sendable(t))
}

pub(crate) fn tray(id: i64) -> Result<Tray, GosError> {
    TRAYS.with(id, "tray", |t| Ok(t.0))
}

pub(crate) fn take_tray(id: i64) -> Result<Tray, GosError> {
    TRAYS.take(id, "tray").map(|t| t.0)
}

pub(crate) fn keep_gl(c: GlContext) -> i64 {
    GL.keep(Sendable(c))
}

pub(crate) fn with_gl<R>(
    id: i64,
    f: impl FnOnce(&GlContext) -> Result<R, GosError>,
) -> Result<R, GosError> {
    GL.with(id, "GL context", |c| f(&c.0))
}

pub(crate) fn drop_gl(id: i64) {
    let _ = GL.take(id, "GL context");
}

pub(crate) fn keep_listener(l: Listener) -> i64 {
    LISTENERS.keep(l)
}

pub(crate) fn with_listener<R>(
    id: i64,
    f: impl FnOnce(&Listener) -> Result<R, GosError>,
) -> Result<R, GosError> {
    LISTENERS.with(id, "instance listener", f)
}

pub(crate) fn take_listener(id: i64) -> Result<Listener, GosError> {
    LISTENERS.take(id, "instance listener")
}
