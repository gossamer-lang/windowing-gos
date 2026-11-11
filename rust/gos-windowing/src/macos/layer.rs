//! The software surface of a window under the Cocoa driver: IOSurfaces shown
//! in turn as the contents of a layer over the content view. The window server
//! composites the surface the program painted, so a frame lives in the buffer
//! the program writes and the one the display reads, with no texture or
//! drawable copies between them.

use std::cell::RefCell;
use std::collections::{HashMap, VecDeque};
use std::ffi::c_void;
use std::time::{Duration, Instant};

use objc2::rc::Retained;
use objc2::runtime::AnyObject;
use objc2_app_kit::NSView;
use objc2_core_foundation::{
    CFNumber, CFRetained, CFString, CFType, CGFloat, CGPoint, CGRect, CGSize,
};
use objc2_io_surface::{
    IOSurfaceLockOptions, IOSurfacePurgeabilityState, IOSurfaceRef, kIOSurfaceBytesPerElement,
    kIOSurfaceBytesPerRow, kIOSurfaceHeight, kIOSurfacePixelFormat, kIOSurfaceWidth,
};
use objc2_quartz_core::{CALayer, CATransaction, kCAFilterNearest, kCAGravityTopLeft};
use sdl3_sys::properties::SDL_GetPointerProperty;
use sdl3_sys::video::{
    SDL_GetWindowProperties, SDL_GetWindowSize, SDL_GetWindowSizeInPixels,
    SDL_PROP_WINDOW_COCOA_WINDOW_POINTER,
};

use crate::display::Rect;
use crate::surface::{PixelFormat, Ring};
use crate::window::Window;
use crate::{Error, Result};

// IOSurface's four-character code for 32-bit pixels stored B, G, R, A.
const BGRA: i32 = i32::from_be_bytes(*b"BGRA");

// The most buffers a window keeps: the one shown, one the window server may
// still be reading for the frame before, and one to paint. A third is made
// only while the other two are both in use.
const POOL: usize = 3;
// How many recent frames' damage a window keeps. A buffer that is a few
// frames old is brought up to date by copying just those areas.
const HISTORY: usize = 8;
// How long a window paints nothing before it keeps only the buffer it shows.
// Shorter than half a caret blink, so a blinking caret, which changes a cell
// at a time, holds one buffer between its paints rather than every spare.
const QUIET: Duration = Duration::from_millis(250);

struct Buffer {
    surface: CFRetained<IOSurfaceRef>,
    // The frame the buffer holds; a new buffer holds none.
    frame: Option<u64>,
    // A volatile buffer's pages are the system's to reclaim.
    volatile: bool,
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
    // One refresh of the window's display.
    refresh: Duration,
    // When to look again at a buffer the window server still reads, and the
    // wait that led there.
    recheck: Option<(Instant, Duration)>,
    last_paint: Instant,
    // Rings shown below the surface. The surface is transparent over them.
    rings: Vec<RingLayer>,
}

// How many recent versions of a ring's content are kept, with the rows each
// one changed. An older buffer is brought up to date by copying those rows.
const RING_HISTORY: usize = 8;

// A ring of rows shown as two layers below the window's surface. The upper
// layer shows stored rows `shift..`, the lower one rows `..shift`. Scrolling
// changes which part of the buffer each layer shows; no pixels move.
struct RingLayer {
    key: u64,
    width: i64,
    height: i64,
    upper: Retained<CALayer>,
    lower: Retained<CALayer>,
    buffers: Vec<RingBuffer>,
    front: Option<usize>,
    // Recent content versions: (id, previous id, rows changed), with `None`
    // when the changed rows are unknown.
    history: VecDeque<(u64, u64, Option<Vec<Rect>>)>,
    // The content id, shift and rectangle the layers currently show.
    shown: Option<(u64, i64, Rect)>,
}

struct RingBuffer {
    surface: CFRetained<IOSurfaceRef>,
    // The content id this buffer holds, or `None` for a new buffer.
    content: Option<u64>,
}

impl Drop for RingLayer {
    fn drop(&mut self) {
        self.upper.removeFromSuperlayer();
        self.lower.removeFromSuperlayer();
    }
}

impl RingLayer {
    fn new(key: u64, width: i64, height: i64, host: &CALayer, above: &CALayer) -> Self {
        let band = || {
            let layer = CALayer::new();
            layer.setOpaque(true);
            // The ring is shown at its own size, one stored pixel per window
            // pixel, so turn off filtering to keep it sharp.
            // SAFETY: the filter is a constant Core Animation defines.
            unsafe {
                layer.setMagnificationFilter(kCAFilterNearest);
                layer.setMinificationFilter(kCAFilterNearest);
            }
            layer.setHidden(true);
            host.insertSublayer_below(&layer, Some(above));
            layer
        };
        Self {
            key,
            width,
            height,
            upper: band(),
            lower: band(),
            buffers: Vec::new(),
            front: None,
            history: VecDeque::new(),
            shown: None,
        }
    }

    // The rows that changed between content `held` and content `now`, or
    // `None` if the history does not reach back that far.
    fn missing(&self, held: Option<u64>, now: u64) -> Option<Vec<Rect>> {
        let held = held?;
        let mut written = Vec::new();
        let mut at = now;
        for _ in 0..self.history.len() + 1 {
            if at == held {
                return Some(written);
            }
            let (_, previous, rows) = self.history.iter().find(|(id, _, _)| *id == at)?;
            written.extend_from_slice(rows.as_deref()?);
            at = *previous;
        }
        None
    }

    // Picks a buffer to write. Prefer a buffer the window server is not
    // reading, and among those the newest, since it has the fewest rows to
    // copy. Otherwise make a new one if the pool has room, or reuse the
    // oldest.
    fn target(&mut self) -> Result<usize> {
        let front = self.front;
        if let Some(i) = (0..self.buffers.len())
            .filter(|&i| Some(i) != front && !self.buffers[i].surface.is_in_use())
            .max_by_key(|&i| self.buffers[i].content)
        {
            return Ok(i);
        }
        if self.buffers.len() < POOL {
            let made = buffer(self.width, self.height)?;
            self.buffers.push(RingBuffer {
                surface: made.surface,
                content: None,
            });
            return Ok(self.buffers.len() - 1);
        }
        (0..self.buffers.len())
            .filter(|&i| Some(i) != front)
            .min_by_key(|&i| self.buffers[i].content)
            .ok_or_else(|| Error::new("the ring has no buffer to write"))
    }

    // Shows `ring` in a window `height` pixels tall at `scale` pixels per
    // point. Copies only the rows the chosen buffer is missing.
    fn show(&mut self, ring: &Ring<'_>, scale: CGFloat, height: i64) -> Result<()> {
        if !self.history.iter().any(|(id, _, _)| *id == ring.id) {
            self.history
                .push_back((ring.id, ring.previous, ring.written.map(<[Rect]>::to_vec)));
            if self.history.len() > RING_HISTORY {
                self.history.pop_front();
            }
        }
        let shift = ring.shift.rem_euclid(self.height.max(1));
        if self.shown == Some((ring.id, shift, ring.rect)) {
            return Ok(());
        }
        let held = self.front.and_then(|f| self.buffers[f].content);
        if held != Some(ring.id) {
            let back = self.target()?;
            let missing = self.missing(self.buffers[back].content, ring.id);
            {
                let mut target =
                    Locked::new(&self.buffers[back].surface, IOSurfaceLockOptions::empty())?;
                // Mark each changed row once, so a row changed in several
                // versions is copied only once. Rows are copied whole.
                let mut stale = vec![missing.is_none(); self.height.max(0) as usize];
                for r in missing.iter().flatten() {
                    if let Some(r) = clip(*r, self.width, self.height) {
                        stale[r.y as usize..(r.y + r.height) as usize].fill(true);
                    }
                }
                let bytes = (self.width * 4) as usize;
                for (row, _) in stale.iter().enumerate().filter(|(_, s)| **s) {
                    let at = row * ring.stride as usize;
                    let Some(from) = ring.pixels.get(at..at + bytes) else {
                        continue;
                    };
                    target
                        .row_mut(row as i64, 0, self.width)
                        .copy_from_slice(from);
                }
            }
            self.buffers[back].content = Some(ring.id);
            self.front = Some(back);
            let surface = &self.buffers[back].surface;
            // SAFETY: IOSurfaceRef is toll-free bridged to the IOSurface object
            // that a layer takes as contents; the layers retain it.
            unsafe {
                let contents = CFRetained::as_ptr(surface).as_ptr().cast::<AnyObject>();
                self.upper.setContents(Some(&*contents));
                self.lower.setContents(Some(&*contents));
            }
        }
        // Show stored rows `shift..` at the top and rows `..shift` below them.
        let r = ring.rect;
        let above = self.height - shift;
        place(
            &self.upper,
            (r.x, r.y, r.width, above),
            (shift, above),
            self.height,
            scale,
            height,
        );
        place(
            &self.lower,
            (r.x, r.y + above, r.width, shift),
            (0, shift),
            self.height,
            scale,
            height,
        );
        self.shown = Some((ring.id, shift, ring.rect));
        Ok(())
    }

    // Frees every buffer except the one on screen.
    fn trim(&mut self) {
        let Some(front) = self.front else {
            self.buffers.clear();
            return;
        };
        if self.buffers.len() > 1 {
            let shown = self.buffers.swap_remove(front);
            self.buffers = vec![shown];
            self.front = Some(0);
        }
    }
}

// Makes `layer` show rows `from..from + rows` of a buffer `stored` rows tall,
// placed at window pixels `(x, y, width, rows)`. The window is `height`
// pixels tall at `scale` pixels per point. Hides the layer if `rows` is 0.
fn place(
    layer: &CALayer,
    (x, y, width, rows): (i64, i64, i64, i64),
    (from, _): (i64, i64),
    stored: i64,
    scale: CGFloat,
    height: i64,
) {
    if rows <= 0 || stored <= 0 {
        layer.setHidden(true);
        return;
    }
    // If the host layer is flipped, y grows downward; otherwise upward. The
    // contents rectangle uses the same direction.
    let down = flipped_override(layer.contentsAreFlipped(), "GOS_RING_DOWN");
    let top = if down { y } else { height - (y + rows) };
    layer.setContentsScale(scale);
    layer.setFrame(CGRect::new(
        CGPoint::new(x as CGFloat / scale, top as CGFloat / scale),
        CGSize::new(width as CGFloat / scale, rows as CGFloat / scale),
    ));
    let contents_down = flipped_override(down, "GOS_RING_CONTENTS_DOWN");
    let start = if contents_down {
        from
    } else {
        stored - (from + rows)
    };
    layer.setContentsRect(CGRect::new(
        CGPoint::new(0.0, start as CGFloat / stored as CGFloat),
        CGSize::new(1.0, rows as CGFloat / stored as CGFloat),
    ));
    layer.setHidden(false);
}

// Returns `value`, flipped if the environment variable `name` is "1". This
// lets a tester correct the layer orientation on a system that reports it
// wrongly, without rebuilding.
fn flipped_override(value: bool, name: &str) -> bool {
    value ^ (std::env::var(name).as_deref() == Ok("1"))
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
/// the shown frame changed outside `damage`, then shows it. `paint` must
/// write every pixel of `damage`; empty `damage` repaints every pixel. Any
/// rings the window showed are removed.
pub(crate) fn paint(
    window: &Window,
    damage: &[Rect],
    paint: impl FnOnce(&mut [u8], i64, i64, i64, PixelFormat),
) -> Result<()> {
    CATransaction::begin();
    CATransaction::setDisableActions(true);
    let painted = paint_main(window, damage, paint).and_then(|()| {
        SURFACES.with_borrow_mut(|surfaces| match surfaces.get_mut(&window.id.0) {
            Some(surface) => surface.show_rings(window, &[]),
            None => Ok(()),
        })
    });
    CATransaction::commit();
    painted
}

/// Like `paint`, and also shows `rings` in layers below the window's
/// surface. Everything happens in one transaction, so the screen never shows
/// the surface from one frame with rings from another. With `damage` of
/// `None`, the surface is left as it is.
pub(crate) fn paint_rings(
    window: &Window,
    damage: Option<&[Rect]>,
    rings: &[Ring<'_>],
    paint: impl FnOnce(&mut [u8], i64, i64, i64, PixelFormat),
) -> Result<()> {
    CATransaction::begin();
    CATransaction::setDisableActions(true);
    let painted = match damage {
        Some(damage) => paint_main(window, damage, paint),
        None => Ok(()),
    }
    .and_then(|()| {
        let size = pixel_size(window);
        SURFACES.with_borrow_mut(|surfaces| match surfaces.get_mut(&window.id.0) {
            // After a resize the surface is repainted in full before any rings.
            Some(surface) if (surface.width, surface.height) == size => {
                surface.show_rings(window, rings)
            }
            _ => Ok(()),
        })
    });
    CATransaction::commit();
    painted
}

fn paint_main(
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
        // Blend the surface over the rings, the way the screen shows them.
        // The surface's pixels are premultiplied, so this is plain "over".
        for ring in &surface.rings {
            let (Some(front), Some((_, shift, r))) = (ring.front, ring.shown) else {
                continue;
            };
            let under = Locked::new(&ring.buffers[front].surface, IOSurfaceLockOptions::ReadOnly)?;
            for y in 0..r.height {
                let row = r.y + y;
                if row < 0 || row >= surface.height {
                    continue;
                }
                let stored = (y + shift).rem_euclid(ring.height);
                let from = under.row(stored, 0, ring.width);
                for x in 0..r.width {
                    let column = r.x + x;
                    if column < 0 || column >= surface.width {
                        continue;
                    }
                    let at = (row * stride + column * 4) as usize;
                    let keep = 255 - u32::from(pixels[at + 3]);
                    for c in 0..4 {
                        let below = u32::from(from[(x * 4) as usize + c]);
                        pixels[at + c] =
                            (u32::from(pixels[at + c]) + (below * keep + 127) / 255).min(255) as u8;
                    }
                }
            }
        }
        Ok(Some(Captured {
            pixels,
            width: surface.width,
            height: surface.height,
            stride,
        }))
    })
}

/// Hands every buffer a window no longer shows back to the system to
/// reclaim, once the window server has stopped reading it. Answers how long
/// the caller may wait before calling again while it still reads one.
pub(crate) fn retire() -> Option<Duration> {
    let now = Instant::now();
    SURFACES.with_borrow_mut(|surfaces| {
        surfaces
            .values_mut()
            .filter_map(|surface| surface.settle(now))
            .min()
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
            refresh: refresh(window),
            recheck: None,
            last_paint: Instant::now(),
            rings: Vec::new(),
        })
    }

    // Shows `rings` below the surface and removes rings no longer listed.
    // The surface is marked opaque only when there are no rings.
    fn show_rings(&mut self, window: &Window, rings: &[Ring<'_>]) -> Result<()> {
        self.rings
            .retain(|shown| rings.iter().any(|r| r.key == shown.key));
        if rings.is_empty() {
            self.layer.setOpaque(true);
            return Ok(());
        }
        let host = self
            .layer
            .superlayer()
            .ok_or_else(|| Error::new("the window's layer has no host"))?;
        let (points_w, _) = point_size(window);
        let scale = if points_w > 0 {
            self.width as CGFloat / points_w as CGFloat
        } else {
            1.0
        };
        for ring in rings {
            let r = ring.rect;
            let size = (r.width, r.height);
            let index = match self.rings.iter().position(|s| s.key == ring.key) {
                Some(i) if (self.rings[i].width, self.rings[i].height) == size => i,
                found => {
                    if let Some(i) = found {
                        self.rings.remove(i);
                    }
                    self.rings.push(RingLayer::new(
                        ring.key,
                        r.width,
                        r.height,
                        &host,
                        &self.layer,
                    ));
                    self.rings.len() - 1
                }
            };
            self.rings[index].show(ring, scale, self.height)?;
        }
        self.layer.setOpaque(false);
        self.last_paint = Instant::now();
        Ok(())
    }

    // Retires the buffers this window no longer shows, checking again one
    // refresh later, then at twice each previous wait, while the window
    // server still reads one.
    fn settle(&mut self, now: Instant) -> Option<Duration> {
        if let Some((due, _)) = self.recheck
            && now < due
        {
            return Some(due - now);
        }
        let mut reading = false;
        for (i, buffer) in self.buffers.iter_mut().enumerate() {
            if Some(i) == self.front || buffer.volatile {
                continue;
            }
            if buffer.surface.is_in_use() {
                reading = true;
            } else {
                buffer.volatile = purgeable(
                    &buffer.surface,
                    IOSurfacePurgeabilityState::PurgeableVolatile,
                )
                .is_some();
            }
        }
        if !reading {
            self.recheck = None;
            return self.release_spares(now);
        }
        let wait = self.recheck.map_or(self.refresh, |(_, wait)| wait * 2);
        self.recheck = Some((now + wait, wait));
        Some(wait)
    }

    // Frees every buffer but the shown one once the window has gone quiet,
    // since a volatile surface still counts toward the process's footprint;
    // the next paint makes a buffer and copies the shown frame into it.
    // Answers how long until that is due.
    fn release_spares(&mut self, now: Instant) -> Option<Duration> {
        let front = self.front?;
        let spare = |s: &Self| s.buffers.len() > 1 || s.rings.iter().any(|r| r.buffers.len() > 1);
        if !spare(self) {
            return None;
        }
        let quiet = now.saturating_duration_since(self.last_paint);
        if quiet < QUIET {
            return Some(QUIET - quiet);
        }
        let shown = self.buffers.swap_remove(front);
        self.buffers = vec![shown];
        self.front = Some(0);
        for ring in &mut self.rings {
            ring.trim();
        }
        None
    }

    // Takes buffer `index` back from the system before it is painted; a
    // buffer whose pages the system reclaimed holds no frame.
    fn reclaim(&mut self, index: usize) {
        let buffer = &mut self.buffers[index];
        if !buffer.volatile {
            return;
        }
        buffer.volatile = false;
        if purgeable(
            &buffer.surface,
            IOSurfacePurgeabilityState::PurgeableNonVolatile,
        ) == Some(IOSurfacePurgeabilityState::PurgeableEmpty)
        {
            buffer.frame = None;
        }
    }

    // Picks a buffer to paint. Prefer a buffer the window server is not
    // reading, and among those the newest, since it has the least to copy.
    // Otherwise make a new one if the pool has room, or reuse the oldest.
    fn target(&mut self) -> Result<usize> {
        let front = self.front;
        let idle = (0..self.buffers.len())
            .filter(|&i| Some(i) != front && !self.buffers[i].surface.is_in_use())
            .max_by_key(|&i| self.buffers[i].frame);
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

    fn paint(
        &mut self,
        window: &Window,
        damage: &[Rect],
        paint: impl FnOnce(&mut [u8], i64, i64, i64, PixelFormat),
    ) -> Result<()> {
        self.last_paint = Instant::now();
        let back = self.target()?;
        self.reclaim(back);
        {
            let mut target =
                Locked::new(&self.buffers[back].surface, IOSurfaceLockOptions::empty())?;
            if let Some(front) = self.front {
                let source =
                    Locked::new(&self.buffers[front].surface, IOSurfaceLockOptions::ReadOnly)?;
                let whole = Rect {
                    x: 0,
                    y: 0,
                    width: self.width,
                    height: self.height,
                };
                let held = self.buffers[back].frame;
                for r in crate::catch_up::missing(&self.history, held, whole, damage) {
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
        // Keep enough history that a buffer the window server held for a few
        // frames can still be updated by copying only what changed.
        if self.history.len() > HISTORY {
            self.history.pop_front();
        }
        self.show(window, back);
        self.front = Some(back);
        self.refresh = refresh(window);
        self.recheck = None;
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

// One refresh of the window's display, or of a 60 Hz one when it reports
// no rate.
fn refresh(window: &Window) -> Duration {
    let rate = window
        .display()
        .map_or(0.0, |display| display.mode.refresh_rate);
    Duration::from_secs_f64(1.0 / if rate > 0.0 { rate } else { 60.0 })
}

// Sets how the system may treat `surface`'s pages; answers the state they
// were in, or `None` when the system refused.
fn purgeable(
    surface: &IOSurfaceRef,
    state: IOSurfacePurgeabilityState,
) -> Option<IOSurfacePurgeabilityState> {
    let mut old = 0;
    // SAFETY: the surface is live and `old` is storage the call fills.
    let status = unsafe { surface.set_purgeable(state.0, &mut old) };
    (status == 0).then_some(IOSurfacePurgeabilityState(old))
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
        volatile: false,
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
