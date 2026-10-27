//! Desktop notifications: the session's notification server on Linux
//! (`org.freedesktop.Notifications`), `NSUserNotificationCenter` on macOS (for
//! a program run from an application bundle), and a notification-area balloon
//! on Windows, which Windows 10 and later show as a toast.

use crate::{App, Result};

/// How urgent a notification is.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
pub enum Urgency {
    Low,
    #[default]
    Normal,
    Critical,
}

/// A desktop notification. `timeout_ms` -1 leaves the duration to the server.
#[derive(Debug, Clone, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
pub struct Notification {
    pub title: String,
    pub body: String,
    pub urgency: Urgency,
    pub timeout_ms: i64,
}

impl Default for Notification {
    fn default() -> Notification {
        Notification {
            title: String::new(),
            body: String::new(),
            urgency: Urgency::Normal,
            timeout_ms: -1,
        }
    }
}

impl App {
    /// Shows `notification` through the desktop's notification service.
    #[cfg(target_os = "linux")]
    pub fn notify(&self, notification: &Notification) -> Result<()> {
        send_linux(&app_name(), notification)
    }

    /// Shows `notification` and calls `done` with the outcome. On Linux the
    /// server is asked from a thread of its own, since D-Bus may first start
    /// the server, so `done` runs on that thread; elsewhere it runs before
    /// this returns.
    #[cfg(target_os = "linux")]
    pub fn notify_then(
        &self,
        notification: Notification,
        done: impl FnOnce(Result<()>) + Send + 'static,
    ) {
        let name = app_name();
        // The thread takes `done` when it starts; a thread that never starts
        // leaves it here to report that.
        let slot = std::sync::Arc::new(parking_lot::Mutex::new(Some(done)));
        let theirs = slot.clone();
        let spawned = std::thread::Builder::new()
            .name("gos-windowing-notify".into())
            .spawn(move || {
                let done = theirs.lock().take();
                if let Some(done) = done {
                    done(send_linux(&name, &notification))
                }
            });
        if let Err(e) = spawned
            && let Some(done) = slot.lock().take()
        {
            done(Err(crate::Error::new(format!(
                "could not start the notification thread: {e}"
            ))))
        }
    }

    /// Shows `notification` and calls `done` with the outcome before this returns.
    #[cfg(not(target_os = "linux"))]
    pub fn notify_then(
        &self,
        notification: Notification,
        done: impl FnOnce(Result<()>) + Send + 'static,
    ) {
        done(self.notify(&notification))
    }

    /// Shows `notification` through the desktop's notification service.
    #[cfg(target_os = "macos")]
    pub fn notify(&self, notification: &Notification) -> Result<()> {
        crate::support::require_main_thread("App::notify");
        crate::macos::deliver_notification(&notification.title, &notification.body)
    }

    /// Shows `notification` through the desktop's notification service.
    #[cfg(target_os = "windows")]
    pub fn notify(&self, notification: &Notification) -> Result<()> {
        crate::support::require_main_thread("App::notify");
        let severity = match notification.urgency {
            Urgency::Low | Urgency::Normal => 0,
            Urgency::Critical => 2,
        };
        let windows =
            crate::window::native_handles(sdl3_sys::video::SDL_PROP_WINDOW_WIN32_HWND_POINTER);
        crate::win32::show_notification(&notification.title, &notification.body, severity, &windows)
    }

    /// Notifications need a platform adapter this platform lacks.
    #[cfg(not(any(target_os = "linux", target_os = "macos", target_os = "windows")))]
    pub fn notify(&self, _notification: &Notification) -> Result<()> {
        Err(crate::Error::new(
            "desktop notifications are not available on this platform",
        ))
    }
}

/// Whether `App::notify` has an adapter on this platform.
pub fn has_notifications() -> bool {
    cfg!(any(
        target_os = "linux",
        target_os = "macos",
        target_os = "windows"
    ))
}

#[cfg(target_os = "linux")]
fn app_name() -> String {
    use sdl3_sys::init::{SDL_GetAppMetadataProperty, SDL_PROP_APP_METADATA_NAME_STRING};
    // SAFETY: SDL answers a string it owns, or null.
    crate::support::read_text(unsafe {
        SDL_GetAppMetadataProperty(SDL_PROP_APP_METADATA_NAME_STRING)
    })
}

#[cfg(target_os = "linux")]
fn send_linux(name: &str, notification: &Notification) -> Result<()> {
    use std::collections::HashMap;

    use zbus::zvariant::Value;

    let connection = zbus::blocking::Connection::session()
        .map_err(|e| crate::Error::new(format!("the session bus is unavailable: {e}")))?;
    let urgency: u8 = match notification.urgency {
        Urgency::Low => 0,
        Urgency::Normal => 1,
        Urgency::Critical => 2,
    };
    let mut hints: HashMap<&str, Value<'_>> = HashMap::new();
    hints.insert("urgency", Value::U8(urgency));
    let actions: Vec<&str> = Vec::new();
    connection
        .call_method(
            Some("org.freedesktop.Notifications"),
            "/org/freedesktop/Notifications",
            Some("org.freedesktop.Notifications"),
            "Notify",
            &(
                name,
                0u32,
                "",
                notification.title.as_str(),
                notification.body.as_str(),
                actions,
                hints,
                notification.timeout_ms as i32,
            ),
        )
        .map_err(|e| {
            crate::Error::new(format!(
                "the notification server refused the notification: {e}"
            ))
        })?;
    Ok(())
}
