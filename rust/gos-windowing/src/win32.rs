//! Win32 glue: the dark frame, the Windows 11 backdrop, and notification-area
//! balloons. Off Windows each answers that it needs Windows.

#[cfg(target_os = "windows")]
mod native;
#[cfg(target_os = "windows")]
pub(crate) use native::*;

#[cfg(not(target_os = "windows"))]
mod fallback {
    use std::ffi::c_void;

    use crate::{Error, Result};

    pub(crate) fn set_dark_frame(_hwnd: *mut c_void, _dark: bool) -> Result<()> {
        Err(Error::new("a Windows frame needs Windows"))
    }

    pub(crate) fn set_backdrop(_hwnd: *mut c_void, _kind: i32) -> Result<()> {
        Err(Error::new("a window backdrop is a Windows feature"))
    }

    pub(crate) fn shutdown() {}
}

#[cfg(not(target_os = "windows"))]
pub(crate) use fallback::*;
