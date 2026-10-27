//! The clipboard: text, data under MIME types, and the primary selection
//! X11 and Wayland keep beside it (what middle-click pastes).

use std::ffi::{CString, c_char, c_void};

use sdl3_sys::clipboard::*;
use sdl3_sys::init::{SDL_INIT_VIDEO, SDL_WasInit};
use sdl3_sys::stdinc::{SDL_free, SDL_malloc};

use crate::{Error, Result, support};

pub fn text() -> Result<String> {
    support::require_main_thread("windowing::clipboard::text");
    // SAFETY: SDL answers a string for the caller to free, or null.
    let p = unsafe { SDL_GetClipboardText() };
    if p.is_null() {
        return Err(support::failure("reading the clipboard"));
    }
    Ok(support::take_text(p))
}

pub fn set_text(text: &str) -> Result<()> {
    support::require_main_thread("windowing::clipboard::set_text");
    let bytes = support::c_text(text, "clipboard text")?;
    // SAFETY: SDL copies the string.
    support::check(
        unsafe { SDL_SetClipboardText(bytes.as_ptr()) },
        "setting the clipboard",
    )
}

pub fn has_text() -> bool {
    support::require_main_thread("windowing::clipboard::has_text");
    // SAFETY: plain query.
    unsafe { SDL_HasClipboardText() }
}

pub fn primary_text() -> Result<String> {
    support::require_main_thread("windowing::clipboard::primary_text");
    // SAFETY: SDL answers a string for the caller to free, or null.
    let p = unsafe { SDL_GetPrimarySelectionText() };
    if p.is_null() {
        return Err(support::failure("reading the primary selection"));
    }
    Ok(support::take_text(p))
}

pub fn set_primary_text(text: &str) -> Result<()> {
    support::require_main_thread("windowing::clipboard::set_primary_text");
    let bytes = support::c_text(text, "primary selection text")?;
    // SAFETY: SDL copies the string.
    support::check(
        unsafe { SDL_SetPrimarySelectionText(bytes.as_ptr()) },
        "setting the primary selection",
    )
}

pub fn has_primary_text() -> bool {
    support::require_main_thread("windowing::clipboard::has_primary_text");
    // SAFETY: plain query.
    unsafe { SDL_HasPrimarySelectionText() }
}

/// The MIME types the clipboard holds now.
pub fn mime_types() -> Result<Vec<String>> {
    support::require_main_thread("windowing::clipboard::mime_types");
    let mut count = 0usize;
    // SAFETY: count receives the length; SDL answers a list to free, or null.
    let list = unsafe { SDL_GetClipboardMimeTypes(&mut count) };
    if list.is_null() {
        return Ok(Vec::new());
    }
    // SAFETY: SDL answered `count` C strings inside the list's allocation.
    let out = unsafe { std::slice::from_raw_parts(list, count) }
        .iter()
        .map(|p| support::read_text(*p))
        .collect();
    // SAFETY: the list was allocated for the caller to free.
    unsafe { SDL_free(list.cast::<c_void>()) };
    Ok(out)
}

pub fn has_data(mime_type: &str) -> bool {
    support::require_main_thread("windowing::clipboard::has_data");
    match support::c_text(mime_type, "a MIME type") {
        // SAFETY: plain query.
        Ok(name) => unsafe { SDL_HasClipboardData(name.as_ptr()) },
        Err(_) => false,
    }
}

/// The clipboard's bytes under `mime_type`, `None` when it holds none.
pub fn data(mime_type: &str) -> Result<Option<Vec<u8>>> {
    support::require_main_thread("windowing::clipboard::data");
    let name = support::c_text(mime_type, "a MIME type")?;
    let mut size = 0usize;
    // SAFETY: size receives the length; SDL answers a buffer to free, or null.
    let p = unsafe { SDL_GetClipboardData(name.as_ptr(), &mut size) };
    if p.is_null() {
        return Ok(None);
    }
    // SAFETY: SDL answered `size` readable bytes.
    let bytes = unsafe { std::slice::from_raw_parts(p.cast::<u8>(), size) }.to_vec();
    // SAFETY: the buffer was allocated for the caller to free.
    unsafe { SDL_free(p) };
    Ok(Some(bytes))
}

// SDL asks for one type at a time, as another application pastes; it frees
// each answered buffer itself.
unsafe extern "C" fn offer_data(
    data: *mut c_void,
    mime_type: *const c_char,
    size: *mut usize,
) -> *const c_void {
    // SAFETY: data is the Offer `set_data` leaked for SDL, alive until `end_offer`.
    let items = unsafe { &*data.cast::<Vec<(String, Vec<u8>)>>() };
    let wanted = support::read_text(mime_type);
    for (kind, bytes) in items {
        if *kind == wanted {
            // SAFETY: SDL frees the buffer with SDL_free once it is done.
            let copy = unsafe { SDL_malloc(bytes.len().max(1)) }.cast::<u8>();
            if copy.is_null() {
                break;
            }
            // SAFETY: copy holds at least bytes.len() bytes; size is SDL's out-parameter.
            unsafe {
                std::ptr::copy_nonoverlapping(bytes.as_ptr(), copy, bytes.len());
                *size = bytes.len();
            }
            return copy.cast();
        }
    }
    // SAFETY: size is SDL's out-parameter.
    unsafe { *size = 0 };
    std::ptr::null()
}

unsafe extern "C" fn end_offer(data: *mut c_void) {
    if !data.is_null() {
        // SAFETY: data is the Box `set_data` leaked; SDL ends each offer once.
        drop(unsafe { Box::from_raw(data.cast::<Vec<(String, Vec<u8>)>>()) });
    }
}

/// Offers `items` (MIME type, bytes) on the clipboard; other applications
/// take whichever type they understand.
pub fn set_data(items: Vec<(String, Vec<u8>)>) -> Result<()> {
    support::require_main_thread("windowing::clipboard::set_data");
    if items.is_empty() {
        return Err(Error::new("an empty offer: give at least one MIME type"));
    }
    // Past this check SDL keeps the offer whatever the call answers, and
    // ends it (running `end_offer`) when it is replaced or cleared.
    // SAFETY: plain query.
    if unsafe { SDL_WasInit(SDL_INIT_VIDEO) }.0 == 0 {
        return Err(Error::new(
            "setting the clipboard data: video is not initialized",
        ));
    }
    // SDL copies the type names, so these C strings go after the call.
    let names = items
        .iter()
        .map(|(kind, _)| support::c_text(kind, "a MIME type"))
        .collect::<Result<Vec<CString>>>()?;
    let pointers: Vec<*const c_char> = names.iter().map(|n| n.as_ptr()).collect();
    let offer = Box::into_raw(Box::new(items)).cast::<c_void>();
    // SAFETY: the callbacks and the leaked offer stay valid until `end_offer`.
    let ok = unsafe {
        SDL_SetClipboardData(
            Some(offer_data),
            Some(end_offer),
            offer,
            pointers.as_ptr().cast_mut(),
            pointers.len(),
        )
    };
    if !ok {
        let error = support::failure("setting the clipboard data");
        // SAFETY: clears whatever SDL kept, ending the offer.
        unsafe { SDL_ClearClipboardData() };
        return Err(error);
    }
    Ok(())
}

/// Empties the clipboard.
pub fn clear() -> Result<()> {
    support::require_main_thread("windowing::clipboard::clear");
    // SAFETY: plain call.
    support::check(
        unsafe { SDL_ClearClipboardData() },
        "clearing the clipboard",
    )
}
