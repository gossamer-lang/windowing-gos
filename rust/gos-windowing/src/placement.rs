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
        // SDL has no Cocoa implementation of SDL_GetWindowBordersSize.
        #[cfg(target_os = "macos")]
        if let Ok(crate::surface::RawHandle::AppKit { ns_window, .. }) = self.raw_handle()
            && let Some((top, left, bottom, right)) = crate::macos::frame_borders(ns_window)
        {
            return Borders {
                top,
                left,
                bottom,
                right,
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
    /// does not settle it in time; that delay is the error returned. A
    /// hidden window has the frame's size when this returns.
    pub fn set_frame(&self, frame: Rect) -> Result<()> {
        support::require_main_thread("Window::set_frame");
        let mut left_state = false;
        if self.is_fullscreen() {
            self.set_fullscreen(false)?;
            left_state = true;
        }
        if self.is_maximized() {
            self.restore()?;
            left_state = true;
        }
        // The window manager frames a window it just mapped, and applies a
        // state change, in its own time: until it has, the frame reads as
        // none and a restored size would replace the one set here. AppKit
        // frames a window at once, so on macOS only leaving fullscreen or
        // zoom is waited for: SDL_SyncWindow runs the event loop, which shows
        // the new frame see-through until the window draws at its new size.
        let settled = if cfg!(target_os = "macos") && !left_state {
            Ok(())
        } else {
            self.sync()
        };
        let b = self.borders();
        let content = (
            (frame.width - b.left - b.right).max(1),
            (frame.height - b.top - b.bottom).max(1),
        );
        // X11 takes the move and the resize as one request, which SDL sends
        // as two. A window manager that keeps the window maximized along one
        // side (KWin for a window as tall as the work area) lets go in the
        // same batch, so it never shows the size it would restore.
        #[cfg(free_unix)]
        if self.is_visible()
            && let Ok(crate::surface::RawHandle::Xlib {
                display,
                window,
                screen,
            }) = self.raw_handle()
        {
            crate::xlib::release_maximized(display, window, screen);
            crate::xlib::move_resize(display, window, (frame.x, frame.y), content);
            return settled;
        }
        // SDL leaves the size of a window AppKit reads as zoomed alone, and
        // AppKit takes the whole frame in one call.
        #[cfg(target_os = "macos")]
        if self.is_visible()
            && let Ok(crate::surface::RawHandle::AppKit { ns_window, .. }) = self.raw_handle()
        {
            crate::macos::set_frame(ns_window, frame.x, frame.y, frame.width, frame.height)?;
            return settled;
        }
        self.set_size(content.0, content.1)?;
        self.set_position(frame.x + b.left, frame.y + b.top)?;
        if !self.is_visible() {
            // SDL moves a hidden X11 window once it maps, after the window
            // manager has shown it where it was.
            #[cfg(free_unix)]
            if let Ok(crate::surface::RawHandle::Xlib {
                display, window, ..
            }) = self.raw_handle()
            {
                crate::xlib::move_resize(display, window, (frame.x, frame.y), content);
            }
            // A hidden window draws its first frame at the size SDL holds.
            return self.sync();
        }
        settled
    }

    /// Brings the window in front of the others without taking the keyboard
    /// focus from the window that has it.
    pub fn raise(&self) -> Result<()> {
        support::require_main_thread("Window::raise");
        match self.raw_handle() {
            #[cfg(free_unix)]
            Ok(crate::surface::RawHandle::Xlib {
                display,
                window,
                screen,
            }) => crate::xlib::restack_above(display, window, screen),
            #[cfg(target_os = "macos")]
            Ok(crate::surface::RawHandle::AppKit { ns_window, .. }) => {
                crate::macos::order_front(ns_window)
            }
            #[cfg(target_os = "windows")]
            Ok(crate::surface::RawHandle::Win32 { hwnd, .. }) => crate::win32::raise_inactive(hwnd),
            // A Wayland compositor stacks windows itself.
            _ => {}
        }
        Ok(())
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
