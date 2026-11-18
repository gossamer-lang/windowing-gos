//! What a program needs to lay windows out side by side: the frame the window
//! system draws around a window's content, and whether the window is on the
//! virtual desktop the user is looking at.

use sdl3_sys::video::SDL_GetWindowBordersSize;

use crate::display::Rect;
use crate::window::Window;
use crate::{Result, support};

/// The thickness of the frame around a window's content area, in the
/// logical units window positions and sizes use. The top is usually the
/// title bar.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
pub struct Borders {
    pub top: i64,
    pub left: i64,
    pub bottom: i64,
    pub right: i64,
}

impl Window {
    /// The frame the window system draws around the content area; zero on
    /// each side it does not report (Wayland, a frameless window).
    pub fn borders(&self) -> Borders {
        support::require_main_thread("Window::borders");
        #[cfg(target_os = "windows")]
        if let Ok(crate::surface::RawHandle::Win32 { hwnd, .. }) = self.raw_handle()
            && let Some((top, left, bottom, right)) = crate::win32::frame_borders(hwnd)
        {
            // DWM measures in pixels; positions and sizes are logical units.
            let (logical, _) = self.size();
            let (pixels, _) = self.pixel_size();
            let scale = if logical > 0 && pixels > 0 {
                pixels as f64 / logical as f64
            } else {
                1.0
            };
            let unit = |n: i64| (n as f64 / scale).round() as i64;
            return Borders {
                top: unit(top),
                left: unit(left),
                bottom: unit(bottom),
                right: unit(right),
            };
        }
        let (mut top, mut left, mut bottom, mut right) = (0, 0, 0, 0);
        // SAFETY: handle is live; the four outputs are valid storage.
        let known = unsafe {
            SDL_GetWindowBordersSize(self.handle, &mut top, &mut left, &mut bottom, &mut right)
        };
        if !known {
            return Borders::default();
        }
        Borders {
            top: i64::from(top),
            left: i64::from(left),
            bottom: i64::from(bottom),
            right: i64::from(right),
        }
    }

    /// Puts the window's frame, title bar included, on `frame` in desktop
    /// coordinates. A fullscreen or maximized window returns to a frame of
    /// its own first, as does one the window manager keeps maximized along
    /// one side (X11). The window is placed even when the window manager
    /// does not settle it in time; that delay is the error returned.
    pub fn set_frame(&self, frame: Rect) -> Result<()> {
        support::require_main_thread("Window::set_frame");
        if self.is_fullscreen() {
            self.set_fullscreen(false)?;
        }
        if self.is_maximized() {
            self.restore()?;
        }
        #[cfg(free_unix)]
        if let Ok(crate::surface::RawHandle::Xlib {
            display,
            window,
            screen,
        }) = self.raw_handle()
        {
            crate::xlib::release_maximized(display, window, screen);
        }
        // The window manager frames a window it just mapped, and applies a
        // state change, in its own time: until it has, the frame reads as
        // none and a restored size would replace the one set here.
        let settled = self.sync();
        let b = self.borders();
        self.set_size(
            (frame.width - b.left - b.right).max(1),
            (frame.height - b.top - b.bottom).max(1),
        )?;
        self.set_position(frame.x + b.left, frame.y + b.top)?;
        settled
    }

    /// Whether the window is on the virtual desktop (workspace, Space) the
    /// user is on. A window on every desktop is, and so is every window where
    /// the platform shows applications no desktops (Wayland).
    pub fn on_current_desktop(&self) -> bool {
        support::require_main_thread("Window::on_current_desktop");
        match self.raw_handle() {
            #[cfg(free_unix)]
            Ok(crate::surface::RawHandle::Xlib {
                display,
                window,
                screen,
            }) => crate::xlib::on_current_desktop(display, window, screen),
            #[cfg(target_os = "macos")]
            Ok(crate::surface::RawHandle::AppKit { ns_window, .. }) => {
                crate::macos::on_active_space(ns_window)
            }
            #[cfg(target_os = "windows")]
            Ok(crate::surface::RawHandle::Win32 { hwnd, .. }) => {
                crate::win32::on_current_desktop(hwnd)
            }
            _ => true,
        }
    }
}
