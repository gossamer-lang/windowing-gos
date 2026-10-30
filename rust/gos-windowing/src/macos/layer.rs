//! The software surface of a window under the Cocoa driver: IOSurfaces shown
//! in turn as the contents of a layer over the content view. The window server
//! composites the surface the program painted, so a frame lives in the buffer
//! the program writes and the one the display reads, with no texture or
//! drawable copies between them.

use std::cell::RefCell;
use std::collections::{HashMap, VecDeque};
use std::ffi::c_void;

use objc2::rc::Retained;
use objc2::runtime::AnyObject;
use objc2_app_kit::NSView;
use objc2_core_foundation::{
    CFNumber, CFRetained, CFString, CFType, CGFloat, CGPoint, CGRect, CGSize,
};
use objc2_io_surface::{
    IOSurfaceLockOptions, IOSurfaceRef, kIOSurfaceBytesPerElement, kIOSurfaceBytesPerRow,
    kIOSurfaceHeight, kIOSurfacePixelFormat, kIOSurfaceWidth,
};
use objc2_quartz_core::{CALayer, CATransaction, kCAGravityTopLeft};
use sdl3_sys::properties::SDL_GetPointerProperty;
use sdl3_sys::video::{
    SDL_GetWindowProperties, SDL_GetWindowSize, SDL_GetWindowSizeInPixels,
    SDL_PROP_WINDOW_COCOA_WINDOW_POINTER,
};

use crate::display::Rect;
use crate::surface::PixelFormat;
use crate::window::Window;
use crate::{Error, Result};

// IOSurface's four-character code for 32-bit pixels stored B, G, R, A.
const BGRA: i32 = i32::from_be_bytes(*b"BGRA");

// The most buffers a window keeps: the one shown, one the window server may
// still be reading for the frame before, and one to paint. A third is made
// only while the other two are both in use.
const POOL: usize = 3;

struct Buffer {
    surface: CFRetained<IOSurfaceRef>,
    // The frame the buffer holds; a new buffer holds none.
    frame: Option<u64>,
}

struct LayerSurface {
    layer: Retained<CALayer>,
    width: i64,
    height: i64,
    buffers: Vec<Buffer>,
    // The buffer the layer shows.
    front: Option<usize>,
    // The number of the shown frame.
    frame: u64,
    // What each of the latest frames changed, by frame number, so a buffer
    // holding an earlier frame catches up by copying only those rectangles.
    history: VecDeque<(u64, Vec<Rect>)>,
}

thread_local! {
    static SURFACES: RefCell<HashMap<u32, LayerSurface>> = RefCell::new(HashMap::new());
}

/// Whether the window's software surface is a layer this module keeps.
pub(crate) fn applies() -> bool {
    crate::video_driver() == "cocoa"
}

/// The window's size in pixels.
pub(crate) fn pixel_size(window: &Window) -> (i64, i64) {
    let (mut w, mut h) = (0, 0);
    // SAFETY: the handle is a live window on the main thread.
    if !unsafe { SDL_GetWindowSizeInPixels(window.handle, &mut w, &mut h) } {
        return (0, 0);
    }
    (i64::from(w), i64::from(h))
}

/// Paints into the buffer the layer shows next, which first receives what
/// the shown frame changed, then shows it. `damage` empty repaints every pixel.
pub(crate) fn paint(
    window: &Window,
    damage: &[Rect],
    paint: impl FnOnce(&mut [u8], i64, i64, i64, PixelFormat),
) -> Result<()> {
    let (width, height) = pixel_size(window);
    if width <= 0 || height <= 0 {
        return Ok(());
    }
    let full = Rect {
        x: 0,
        y: 0,
        width,
        height,
    };
    let damage: Vec<Rect> = if damage.is_empty() {
        vec![full]
    } else {
        damage
            .iter()
            .filter_map(|r| clip(*r, width, height))
            .collect()
    };
    SURFACES.with_borrow_mut(|surfaces| {
        let id = window.id.0;
        let fits = surfaces
            .get(&id)
            .is_some_and(|s| s.width == width && s.height == height);
        if !fits {
            // A resized window keeps its layer and takes buffers of the new size.
            let layer = surfaces.remove(&id).map(|s| s.layer);
            surfaces.insert(id, LayerSurface::new(window, width, height, layer)?);
        }
        let Some(surface) = surfaces.get_mut(&id) else {
            return Err(Error::new("the window's layer surface is missing"));
        };
        surface.paint(window, &damage, paint)
    })
}

/// A copy of the pixels a layer shows, `stride` bytes to a row.
pub(crate) struct Captured {
    pub(crate) pixels: Vec<u8>,
    pub(crate) width: i64,
    pub(crate) height: i64,
    pub(crate) stride: i64,
}

/// What the layer shows, or `None` before the first paint.
pub(crate) fn capture(window: &Window) -> Result<Option<Captured>> {
    SURFACES.with_borrow(|surfaces| {
        let Some(surface) = surfaces.get(&window.id.0) else {
            return Ok(None);
        };
        let Some(front) = surface.front else {
            return Ok(None);
        };
        let shown = &surface.buffers[front];
        let locked = Locked::new(&shown.surface, IOSurfaceLockOptions::ReadOnly)?;
        let stride = surface.width * 4;
        let mut pixels = vec![0u8; (stride * surface.height) as usize];
        for row in 0..surface.height {
            let from = locked.row(row, 0, surface.width);
            let at = (row * stride) as usize;
            pixels[at..at + from.len()].copy_from_slice(from);
        }
        Ok(Some(Captured {
            pixels,
            width: surface.width,
            height: surface.height,
            stride,
        }))
    })
}

/// Takes the window's layer and buffers away, as a GPU compositor takes
/// over its drawing or the window closes.
pub(crate) fn release(window: &Window) {
    if let Some(surface) = SURFACES.with_borrow_mut(|s| s.remove(&window.id.0)) {
        surface.layer.removeFromSuperlayer();
    }
}

/// Takes every window's layer and buffers away as the session ends.
pub(crate) fn release_all() {
    for (_, surface) in SURFACES.with_borrow_mut(std::mem::take) {
        surface.layer.removeFromSuperlayer();
    }
}

impl LayerSurface {
    fn new(
        window: &Window,
        width: i64,
        height: i64,
        layer: Option<Retained<CALayer>>,
    ) -> Result<Self> {
        let layer = match layer {
            Some(layer) => layer,
            None => attach(window)?,
        };
        Ok(Self {
            layer,
            width,
            height,
            buffers: Vec::new(),
            front: None,
            frame: 0,
            history: VecDeque::new(),
        })
    }

    // A buffer to paint: one the window server is not reading, a new one
    // while the pool has room, else the one holding the oldest frame.
    fn target(&mut self) -> Result<usize> {
        let front = self.front;
        let idle = (0..self.buffers.len())
            .filter(|&i| Some(i) != front)
            .find(|&i| !self.buffers[i].surface.is_in_use());
        if let Some(i) = idle {
            return Ok(i);
        }
        if self.buffers.len() < POOL {
            self.buffers.push(buffer(self.width, self.height)?);
            return Ok(self.buffers.len() - 1);
        }
        (0..self.buffers.len())
            .filter(|&i| Some(i) != front)
            .min_by_key(|&i| self.buffers[i].frame)
            .ok_or_else(|| Error::new("the layer has no buffer to paint"))
    }

    // What buffer `target` lacks of the shown frame: the rectangles every
    // later frame changed, or all of it when the history no longer reaches
    // back to the frame it holds.
    fn missing(&self, target: usize) -> Vec<Rect> {
        let whole = Rect {
            x: 0,
            y: 0,
            width: self.width,
            height: self.height,
        };
        let Some(held) = self.buffers[target].frame else {
            return vec![whole];
        };
        if self
            .history
            .front()
            .is_none_or(|(first, _)| *first > held + 1)
        {
            return vec![whole];
        }
        self.history
            .iter()
            .filter(|(frame, _)| *frame > held)
            .flat_map(|(_, rects)| rects.iter().copied())
            .collect()
    }

    fn paint(
        &mut self,
        window: &Window,
        damage: &[Rect],
        paint: impl FnOnce(&mut [u8], i64, i64, i64, PixelFormat),
    ) -> Result<()> {
        let back = self.target()?;
        {
            let mut target =
                Locked::new(&self.buffers[back].surface, IOSurfaceLockOptions::empty())?;
            if let Some(front) = self.front {
                let source =
                    Locked::new(&self.buffers[front].surface, IOSurfaceLockOptions::ReadOnly)?;
                for r in self.missing(back) {
                    for row in r.y..r.y + r.height {
                        target
                            .row_mut(row, r.x, r.width)
                            .copy_from_slice(source.row(row, r.x, r.width));
                    }
                }
            }
            let stride = target.stride;
            paint(
                target.bytes_mut(self.height),
                self.width,
                self.height,
                stride,
                PixelFormat::Bgra8,
            );
        }
        self.frame += 1;
        self.buffers[back].frame = Some(self.frame);
        self.history.push_back((self.frame, damage.to_vec()));
        if self.history.len() > POOL {
            self.history.pop_front();
        }
        self.show(window, back);
        self.front = Some(back);
        Ok(())
    }

    // Makes buffer `index` the layer's contents in one transaction without
    // the implicit fade a contents change otherwise animates.
    fn show(&self, window: &Window, index: usize) {
        let (points_w, points_h) = point_size(window);
        let scale = if points_w > 0 {
            self.width as CGFloat / points_w as CGFloat
        } else {
            1.0
        };
        CATransaction::begin();
        CATransaction::setDisableActions(true);
        self.layer.setFrame(CGRect::new(
            CGPoint::new(0.0, 0.0),
            CGSize::new(points_w as CGFloat, points_h as CGFloat),
        ));
        self.layer.setContentsScale(scale);
        let surface = &self.buffers[index].surface;
        // SAFETY: IOSurfaceRef is toll-free bridged to the IOSurface object
        // that a layer takes as contents; the layer retains it.
        unsafe {
            let contents = CFRetained::as_ptr(surface).as_ptr().cast::<AnyObject>();
            self.layer.setContents(Some(&*contents));
        }
        CATransaction::commit();
    }
}

// The window's size in points, the units of its views and layers.
fn point_size(window: &Window) -> (i64, i64) {
    let (mut w, mut h) = (0, 0);
    // SAFETY: the handle is a live window on the main thread.
    if !unsafe { SDL_GetWindowSize(window.handle, &mut w, &mut h) } {
        return (0, 0);
    }
    (i64::from(w), i64::from(h))
}

// Adds an opaque layer over the window's content view and answers it.
fn attach(window: &Window) -> Result<Retained<CALayer>> {
    // SAFETY: the handle is live; SDL answers null when unset.
    let ns_window = unsafe {
        SDL_GetPointerProperty(
            SDL_GetWindowProperties(window.handle),
            SDL_PROP_WINDOW_COCOA_WINDOW_POINTER,
            std::ptr::null_mut(),
        )
    };
    let view: *mut c_void = super::content_view(ns_window);
    if view.is_null() {
        return Err(Error::new("the window has no AppKit content view"));
    }
    // SAFETY: SDL's content view lives as long as its window, which outlives
    // this call on the main thread.
    let view = unsafe { &*view.cast::<NSView>() };
    view.setWantsLayer(true);
    let host = view
        .layer()
        .ok_or_else(|| Error::new("the content view made no layer"))?;
    let layer = CALayer::new();
    // SAFETY: kCAGravityTopLeft is a constant Core Animation defines.
    layer.setContentsGravity(unsafe { kCAGravityTopLeft });
    layer.setOpaque(true);
    host.addSublayer(&layer);
    Ok(layer)
}

// A zeroed BGRA IOSurface of `width` x `height` pixels.
fn buffer(width: i64, height: i64) -> Result<Buffer> {
    let number = |n: i64| CFNumber::new_i64(n);
    let row = IOSurfaceRef::align_property(
        // SAFETY: the key is a constant IOSurface defines.
        unsafe { kIOSurfaceBytesPerRow },
        (width * 4) as usize,
    );
    // SAFETY: the keys are constants IOSurface defines.
    let keys: [&CFString; 5] = unsafe {
        [
            kIOSurfaceWidth,
            kIOSurfaceHeight,
            kIOSurfaceBytesPerElement,
            kIOSurfaceBytesPerRow,
            kIOSurfacePixelFormat,
        ]
    };
    let values = [
        number(width),
        number(height),
        number(4),
        number(row as i64),
        CFNumber::new_i32(BGRA),
    ];
    let values: Vec<&CFType> = values.iter().map(|v| v.as_ref()).collect();
    let keys: Vec<&CFType> = keys.iter().map(|k| k.as_ref()).collect();
    let properties =
        objc2_core_foundation::CFDictionary::<CFType, CFType>::from_slices(&keys, &values);
    // SAFETY: every key names an IOSurface property and every value is the
    // CFNumber it expects.
    let surface = unsafe { IOSurfaceRef::new(properties.as_opaque()) }
        .ok_or_else(|| Error::new(format!("creating a {width}x{height} IOSurface")))?;
    Ok(Buffer {
        surface,
        frame: None,
    })
}

// `r` within a `width` x `height` buffer, or `None` when nothing remains.
fn clip(r: Rect, width: i64, height: i64) -> Option<Rect> {
    let x = r.x.max(0);
    let y = r.y.max(0);
    let right = (r.x + r.width).min(width);
    let bottom = (r.y + r.height).min(height);
    (right > x && bottom > y).then_some(Rect {
        x,
        y,
        width: right - x,
        height: bottom - y,
    })
}

// An IOSurface locked for the CPU until dropped.
struct Locked<'a> {
    surface: &'a IOSurfaceRef,
    options: IOSurfaceLockOptions,
    base: *mut u8,
    stride: i64,
}

impl<'a> Locked<'a> {
    fn new(surface: &'a IOSurfaceRef, options: IOSurfaceLockOptions) -> Result<Self> {
        // SAFETY: the seed pointer may be null.
        let status = unsafe { surface.lock(options, std::ptr::null_mut()) };
        if status != 0 {
            return Err(Error::new(format!(
                "locking an IOSurface failed ({status})"
            )));
        }
        Ok(Self {
            surface,
            options,
            base: surface.base_address().as_ptr().cast::<u8>(),
            stride: surface.bytes_per_row() as i64,
        })
    }

    fn row(&self, row: i64, x: i64, width: i64) -> &[u8] {
        let at = (row * self.stride + x * 4) as usize;
        // SAFETY: callers keep `row` below the surface height and `x + width`
        // within its width, so the span lies inside the locked allocation.
        unsafe { std::slice::from_raw_parts(self.base.add(at), (width * 4) as usize) }
    }

    fn row_mut(&mut self, row: i64, x: i64, width: i64) -> &mut [u8] {
        let at = (row * self.stride + x * 4) as usize;
        // SAFETY: as in `row`; the lock is a write lock.
        unsafe { std::slice::from_raw_parts_mut(self.base.add(at), (width * 4) as usize) }
    }

    fn bytes_mut(&mut self, height: i64) -> &mut [u8] {
        // SAFETY: a locked surface holds `height` rows of `stride` bytes.
        unsafe { std::slice::from_raw_parts_mut(self.base, (self.stride * height) as usize) }
    }
}

impl Drop for Locked<'_> {
    fn drop(&mut self) {
        // SAFETY: the surface was locked with these options by `new`.
        unsafe { self.surface.unlock(self.options, std::ptr::null_mut()) };
    }
}
