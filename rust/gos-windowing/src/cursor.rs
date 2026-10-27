//! The pointer's look: the system's cursors, cursors drawn by the program,
//! and whether the pointer shows at all.

use sdl3_sys::mouse::{
    SDL_CreateColorCursor, SDL_CreateSystemCursor, SDL_Cursor, SDL_CursorVisible,
    SDL_DestroyCursor, SDL_HideCursor, SDL_SetCursor, SDL_ShowCursor, SDL_SystemCursor,
};
use sdl3_sys::surface::SDL_DestroySurface;

use crate::{Result, support, surface};

/// A cursor the system draws.
#[derive(Debug, Clone, Copy, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
pub enum SystemCursor {
    Default,
    Text,
    Wait,
    Crosshair,
    Progress,
    ResizeNwse,
    ResizeNesw,
    ResizeEw,
    ResizeNs,
    Move,
    NotAllowed,
    Pointer,
    ResizeNw,
    ResizeN,
    ResizeNe,
    ResizeE,
    ResizeSe,
    ResizeS,
    ResizeSw,
    ResizeW,
}

/// A created cursor, shown with `show` and freed with `destroy`.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Cursor {
    handle: *mut SDL_Cursor,
}

// SAFETY: a Cursor is an identity; every call that uses it checks it runs
// on the main thread.
unsafe impl Send for Cursor {}

impl Cursor {
    /// Makes this the pointer's cursor.
    pub fn show(&self) -> Result<()> {
        support::require_main_thread("Cursor::show");
        // SAFETY: handle is a live cursor.
        support::check(unsafe { SDL_SetCursor(self.handle) }, "setting the cursor")
    }

    /// Frees the cursor; it must not be used after.
    pub fn destroy(&self) {
        support::require_main_thread("Cursor::destroy");
        // SAFETY: handle is a live cursor not used after this.
        unsafe { SDL_DestroyCursor(self.handle) }
    }
}

/// One of the system's cursors.
pub fn system(kind: SystemCursor) -> Result<Cursor> {
    support::require_main_thread("windowing::cursor::system");
    // SDL_SystemCursor follows the declaration order above.
    // SAFETY: plain creation.
    let handle = unsafe { SDL_CreateSystemCursor(SDL_SystemCursor(kind as i32)) };
    if handle.is_null() {
        return Err(support::failure("creating a system cursor"));
    }
    Ok(Cursor { handle })
}

/// A cursor from `width` x `height` RGBA pixels, its hot spot at (`hot_x`, `hot_y`).
pub fn custom(rgba: &[u8], width: i64, height: i64, hot_x: i64, hot_y: i64) -> Result<Cursor> {
    support::require_main_thread("windowing::cursor::custom");
    let image = surface::rgba_surface(rgba, width, height)?;
    let x = support::c_int(hot_x, "a hot spot x");
    let y = support::c_int(hot_y, "a hot spot y");
    let created = match (x, y) {
        // SAFETY: image is live; SDL copies it.
        (Ok(x), Ok(y)) => Ok(unsafe { SDL_CreateColorCursor(image, x, y) }),
        (Err(e), _) | (_, Err(e)) => Err(e),
    };
    // SAFETY: image was created for this call.
    unsafe { SDL_DestroySurface(image) };
    let handle = created?;
    if handle.is_null() {
        return Err(support::failure("creating a cursor"));
    }
    Ok(Cursor { handle })
}

pub fn show() -> Result<()> {
    support::require_main_thread("windowing::cursor::show");
    // SAFETY: plain call on the main thread.
    support::check(unsafe { SDL_ShowCursor() }, "showing the cursor")
}

pub fn hide() -> Result<()> {
    support::require_main_thread("windowing::cursor::hide");
    // SAFETY: plain call on the main thread.
    support::check(unsafe { SDL_HideCursor() }, "hiding the cursor")
}

pub fn is_visible() -> bool {
    support::require_main_thread("windowing::cursor::is_visible");
    // SAFETY: plain query.
    unsafe { SDL_CursorVisible() }
}
