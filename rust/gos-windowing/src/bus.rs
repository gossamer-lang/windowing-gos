//! The session bus connection the toolkit shares. Preferences, single-instance
//! requests, notifications and the desktop's accessibility switch use one
//! socket and one reader thread rather than a connection each.

use parking_lot::Mutex;
use zbus::blocking::Connection;

static SESSION: Mutex<Option<Connection>> = Mutex::new(None);

/// The shared session bus connection, opened on first use; `None` without a
/// session bus.
pub fn session() -> Option<Connection> {
    let mut held = SESSION.lock();
    if held.is_none() {
        *held = Connection::session().ok();
    }
    held.clone()
}

/// Closes the shared connection as the toolkit shuts down, which ends the
/// streams reading it; the next use opens another.
pub(crate) fn close() {
    if let Some(connection) = SESSION.lock().take() {
        let _ = connection.close();
    }
}
