//! AppKit glue: the content view, native window tabs, the menu bar, frame
//! appearance, drags, notifications, and desktop preferences. Off macOS
//! each answers that it needs macOS.

#[cfg(target_os = "macos")]
mod appkit;
#[cfg(target_os = "macos")]
pub(crate) use appkit::*;

#[cfg(not(target_os = "macos"))]
mod fallback {
    use std::ffi::c_void;

    use crate::drag::{DragId, DragItem, DragOperation};
    use crate::menu::Menu;
    use crate::window::Window;
    use crate::{Error, Result};

    pub(crate) fn content_view(_ns_window: *mut c_void) -> *mut c_void {
        std::ptr::null_mut()
    }

    pub(crate) fn configure_tabs(
        _window: *mut c_void,
        _identifier: &str,
        _enabled: bool,
    ) -> Result<()> {
        Err(Error::new("native window tabs require macOS AppKit"))
    }

    pub(crate) fn merge_tabs(_window: *mut c_void, _other: *mut c_void) -> Result<()> {
        Err(Error::new("native window tabs require macOS AppKit"))
    }

    pub(crate) fn detach_tab(_window: *mut c_void) -> Result<()> {
        Err(Error::new("native window tabs require macOS AppKit"))
    }

    pub(crate) fn install_menu_bar(_menus: &[Menu]) -> Result<()> {
        Err(Error::new(
            "a native menu bar requires macOS; draw the menus in the window",
        ))
    }

    pub(crate) fn set_appearance(_window: *mut c_void, _code: i64) -> Result<()> {
        Err(Error::new("an AppKit appearance needs macOS"))
    }

    pub(crate) fn start_drag(
        _window: Window,
        _items: Vec<DragItem>,
        _operation: DragOperation,
        _finished: u32,
    ) -> Result<DragId> {
        Err(Error::new("AppKit drags need macOS"))
    }
}

#[cfg(not(target_os = "macos"))]
pub(crate) use fallback::*;
