//! Displays: what is connected, where each sits in the desktop, how it is
//! scaled, and the modes exclusive fullscreen can switch it to.

use std::ffi::c_void;

use sdl3_sys::rect::SDL_Rect;
use sdl3_sys::stdinc::SDL_free;
use sdl3_sys::video::{
    SDL_DisplayID, SDL_DisplayMode, SDL_GetCurrentDisplayMode, SDL_GetCurrentDisplayOrientation,
    SDL_GetDisplayBounds, SDL_GetDisplayContentScale, SDL_GetDisplayName,
    SDL_GetDisplayUsableBounds, SDL_GetDisplays, SDL_GetFullscreenDisplayModes,
    SDL_GetPrimaryDisplay,
};

use crate::Result;
use crate::support;

/// A display, as events name it.
#[derive(
    Debug, Clone, Copy, PartialEq, Eq, Hash, Default, serde::Serialize, serde::Deserialize,
)]
pub struct DisplayId(pub u32);

/// A rectangle in logical desktop coordinates.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default, serde::Serialize, serde::Deserialize)]
pub struct Rect {
    pub x: i64,
    pub y: i64,
    pub width: i64,
    pub height: i64,
}

/// A display mode: resolution, refresh rate, and pixel density.
#[derive(Debug, Clone, Copy, PartialEq, Default, serde::Serialize, serde::Deserialize)]
pub struct Mode {
    pub width: i64,
    pub height: i64,
    pub refresh_rate: f64,
    pub pixel_density: f64,
}

/// How a display is turned.
#[derive(Debug, Clone, Copy, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
pub enum Orientation {
    Unknown,
    Landscape,
    LandscapeFlipped,
    Portrait,
    PortraitFlipped,
}

/// A connected display.
#[derive(Debug, Clone, PartialEq, serde::Serialize, serde::Deserialize)]
pub struct Display {
    pub id: DisplayId,
    pub name: String,
    /// Where the display sits in the desktop.
    pub bounds: Rect,
    /// The part of `bounds` not taken by panels and docks.
    pub usable: Rect,
    /// The content scale the user chose (1.0, 1.5, 2.0, ...).
    pub scale: f64,
    pub mode: Mode,
    pub orientation: Orientation,
    pub primary: bool,
}

pub(crate) fn orientation_from(raw: i32) -> Orientation {
    match raw {
        1 => Orientation::Landscape,
        2 => Orientation::LandscapeFlipped,
        3 => Orientation::Portrait,
        4 => Orientation::PortraitFlipped,
        _ => Orientation::Unknown,
    }
}

/// Every connected display, the primary first.
pub fn all() -> Result<Vec<Display>> {
    support::require_main_thread("windowing::display::all");
    let mut count = 0;
    // SAFETY: count receives the list length; SDL owns nothing after we free.
    let ids = unsafe { SDL_GetDisplays(&mut count) };
    if ids.is_null() {
        return Err(support::failure("listing displays"));
    }
    // SAFETY: SDL answered `count` valid display ids at `ids`.
    let raw: Vec<u32> = unsafe { std::slice::from_raw_parts(ids, count as usize) }
        .iter()
        .map(|id| id.0)
        .collect();
    // SAFETY: the list was allocated for the caller to free.
    unsafe { SDL_free(ids.cast::<c_void>()) };
    // SAFETY: plain query.
    let primary = unsafe { SDL_GetPrimaryDisplay() }.0;
    let ordered = raw
        .iter()
        .copied()
        .filter(|id| *id == primary)
        .chain(raw.iter().copied().filter(|id| *id != primary));
    ordered.map(describe).collect()
}

/// The primary display.
pub fn primary() -> Result<Display> {
    support::require_main_thread("windowing::display::primary");
    // SAFETY: plain query.
    let id = unsafe { SDL_GetPrimaryDisplay() }.0;
    if id == 0 {
        return Err(support::failure("finding the primary display"));
    }
    describe(id)
}

/// The display `id` names.
pub fn get(id: DisplayId) -> Result<Display> {
    support::require_main_thread("windowing::display::get");
    describe(id.0)
}

/// The modes exclusive fullscreen can switch display `id` to.
pub fn modes(id: DisplayId) -> Result<Vec<Mode>> {
    support::require_main_thread("windowing::display::modes");
    let mut count = 0;
    // SAFETY: count receives the list length.
    let list = unsafe { SDL_GetFullscreenDisplayModes(SDL_DisplayID(id.0), &mut count) };
    if list.is_null() {
        return Err(support::failure("listing display modes"));
    }
    // SAFETY: SDL answered `count` mode pointers at `list`.
    let out = unsafe { std::slice::from_raw_parts(list, count as usize) }
        .iter()
        .filter(|p| !p.is_null())
        // SAFETY: each non-null entry points at a mode SDL owns until the free.
        .map(|p| mode_from(unsafe { &**p }))
        .collect();
    // SAFETY: the list was allocated for the caller to free.
    unsafe { SDL_free(list.cast::<c_void>()) };
    Ok(out)
}

pub(crate) fn mode_from(mode: &SDL_DisplayMode) -> Mode {
    Mode {
        width: i64::from(mode.w),
        height: i64::from(mode.h),
        refresh_rate: f64::from(mode.refresh_rate),
        pixel_density: f64::from(mode.pixel_density),
    }
}

pub(crate) fn rect_from(rect: SDL_Rect) -> Rect {
    Rect {
        x: i64::from(rect.x),
        y: i64::from(rect.y),
        width: i64::from(rect.w),
        height: i64::from(rect.h),
    }
}

fn describe(id: u32) -> Result<Display> {
    let sdl_id = SDL_DisplayID(id);
    let mut bounds = SDL_Rect {
        x: 0,
        y: 0,
        w: 0,
        h: 0,
    };
    // SAFETY: bounds is valid storage for the call.
    support::check(
        unsafe { SDL_GetDisplayBounds(sdl_id, &mut bounds) },
        "reading display bounds",
    )?;
    let mut usable = SDL_Rect {
        x: 0,
        y: 0,
        w: 0,
        h: 0,
    };
    support::check(
        // SAFETY: usable is valid storage for the call.
        unsafe { SDL_GetDisplayUsableBounds(sdl_id, &mut usable) },
        "reading usable display bounds",
    )?;
    // SAFETY: SDL answers a mode it owns, or null.
    let mode = unsafe { SDL_GetCurrentDisplayMode(sdl_id) };
    if mode.is_null() {
        return Err(support::failure("reading the display mode"));
    }
    Ok(Display {
        id: DisplayId(id),
        // SAFETY: plain queries on a display SDL just described.
        name: support::read_text(unsafe { SDL_GetDisplayName(sdl_id) }),
        bounds: rect_from(bounds),
        usable: rect_from(usable),
        scale: f64::from(unsafe { SDL_GetDisplayContentScale(sdl_id) }),
        // SAFETY: mode is non-null and owned by SDL for this call.
        mode: mode_from(unsafe { &*mode }),
        orientation: orientation_from(unsafe { SDL_GetCurrentDisplayOrientation(sdl_id) }.0),
        primary: id == unsafe { SDL_GetPrimaryDisplay() }.0,
    })
}
