//! The theme of a window's frame: the title bar and borders the window system
//! draws. Every window starts with the desktop's theme; a program whose own
//! theme differs sets the frame's so the title bar matches its content.

use std::ffi::c_void;

use sdl3_sys::properties::{SDL_GetNumberProperty, SDL_SetNumberProperty};
use sdl3_sys::video::SDL_GetSystemTheme;
use sdl3_sys::video::{SDL_GetWindowID, SDL_GetWindowProperties};

use crate::surface::RawHandle;
use crate::system::Theme;
use crate::window::{Window, WindowId};
use crate::{Error, Result, support};

// The window property holding the program's choice: 0 follows the desktop,
// 1 is light and 2 is dark.
const FRAME_THEME_PROPERTY: &std::ffi::CStr = c"windowing.frame_theme";

fn code_of(theme: Option<Theme>) -> i64 {
    match theme {
        Some(Theme::Light) => 1,
        Some(Theme::Dark) => 2,
        Some(Theme::Unknown) | None => 0,
    }
}

impl Window {
    /// Gives the window's frame `theme`, or the desktop's with `None`.
    pub fn set_frame_theme(&self, theme: Option<Theme>) -> Result<()> {
        support::require_main_thread("Window::set_frame_theme");
        let code = code_of(theme);
        support::check(
            // SAFETY: handle is live.
            unsafe {
                SDL_SetNumberProperty(
                    SDL_GetWindowProperties(self.handle),
                    FRAME_THEME_PROPERTY.as_ptr(),
                    code,
                )
            },
            "keeping the frame theme",
        )?;
        self.apply_frame_theme(code)
    }

    fn apply_frame_theme(&self, code: i64) -> Result<()> {
        match self.raw_handle()? {
            RawHandle::Win32 { hwnd, .. } => {
                // SAFETY: plain query.
                let dark = code == 2 || (code == 0 && unsafe { SDL_GetSystemTheme() }.0 == 2);
                crate::win32::set_dark_frame(hwnd, dark)
            }
            RawHandle::AppKit { ns_window, .. } => crate::macos::set_appearance(ns_window, code),
            RawHandle::Xlib {
                display, window, ..
            } => set_theme_variant(display, window, code),
            RawHandle::Wayland { .. } => Err(Error::new(
                "on Wayland the compositor draws the frame in its own theme",
            )),
            RawHandle::Drm { .. } => Err(Error::new(
                "a display without a window system draws no frame",
            )),
        }
    }
}

/// Applies each window's chosen frame theme again, after the desktop's changed.
pub(crate) fn reapply() {
    for handle in crate::window::all_windows() {
        // SAFETY: handle is an open window.
        let code = unsafe {
            SDL_GetNumberProperty(
                SDL_GetWindowProperties(handle),
                FRAME_THEME_PROPERTY.as_ptr(),
                0,
            )
        };
        if code != 0 {
            // SAFETY: handle is an open window.
            let id = WindowId(unsafe { SDL_GetWindowID(handle) }.0);
            let window = Window {
                handle,
                id,
                redraw_type: 0,
            };
            // A window whose frame cannot take the theme keeps the desktop's.
            let _ = window.apply_frame_theme(code);
        }
    }
}

// X11 window managers that draw GTK-style frames read the variant from the
// `_GTK_THEME_VARIANT` property of the client window; no property means the
// desktop's theme.
#[cfg(target_os = "linux")]
fn set_theme_variant(display: *mut c_void, window: u64, code: i64) -> Result<()> {
    let x = crate::xlib::xlib().ok_or_else(|| Error::new("loading the X11 library"))?;
    let display = display.cast::<x11_dl::xlib::Display>();
    let property = crate::xlib::atom(x, display, "_GTK_THEME_VARIANT");
    // SAFETY: display and window are SDL's live X11 objects; data outlives the call.
    unsafe {
        if code == 0 {
            (x.XDeleteProperty)(display, window, property);
        } else {
            let utf8 = crate::xlib::atom(x, display, "UTF8_STRING");
            let value: &[u8] = if code == 2 { b"dark" } else { b"light" };
            (x.XChangeProperty)(
                display,
                window,
                property,
                utf8,
                8,
                x11_dl::xlib::PropModeReplace,
                value.as_ptr(),
                value.len() as i32,
            );
        }
        (x.XFlush)(display);
    }
    Ok(())
}

#[cfg(not(target_os = "linux"))]
fn set_theme_variant(_display: *mut c_void, _window: u64, _code: i64) -> Result<()> {
    Err(Error::new("X11 frames need Linux"))
}
