//! The software surface of a window under the Wayland driver: frames painted
//! into shared-memory buffers the compositor reads in place, attached to the
//! window's own `wl_surface`. SDL has no such surface of its own on Wayland
//! and backs one with a GPU renderer, which loads the GL or Vulkan driver into
//! the process.

use std::cell::RefCell;
use std::collections::{HashMap, VecDeque};
use std::os::fd::AsFd;
use std::ptr::NonNull;
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};

use sdl3_sys::properties::{SDL_GetGlobalProperties, SDL_GetPointerProperty};
use sdl3_sys::video::{
    SDL_GetWindowFlags, SDL_GetWindowProperties, SDL_GetWindowSizeInPixels,
    SDL_PROP_WINDOW_WAYLAND_SURFACE_POINTER, SDL_WINDOW_TRANSPARENT,
};
use wayland_backend::client::{Backend, ObjectId};
use wayland_client::protocol::wl_buffer::{self, WlBuffer};
use wayland_client::protocol::wl_registry::{self, WlRegistry};
use wayland_client::protocol::wl_shm::{self, WlShm};
use wayland_client::protocol::wl_shm_pool::WlShmPool;
use wayland_client::protocol::wl_surface::WlSurface;
use wayland_client::{Connection, Dispatch, EventQueue, Proxy, QueueHandle, delegate_noop};

use crate::display::Rect;
use crate::surface::PixelFormat;
use crate::window::Window;
use crate::{Error, Result};

// The most buffers a window keeps: the one shown, one the compositor may
// still be reading for the frame before, and one to paint. A second and a
// third are made only while the ones before are all in use.
const POOL: usize = 3;

// The surface version that takes damage in buffer pixels.
const DAMAGE_BUFFER_VERSION: u32 = 4;

#[derive(Default)]
struct Globals {
    shm: Option<(u32, u32)>,
}

impl Dispatch<WlRegistry, ()> for Globals {
    fn event(
        globals: &mut Self,
        _: &WlRegistry,
        event: wl_registry::Event,
        _: &(),
        _: &Connection,
        _: &QueueHandle<Self>,
    ) {
        if let wl_registry::Event::Global {
            name,
            interface,
            version,
        } = event
            && interface == "wl_shm"
            && globals.shm.is_none()
        {
            globals.shm = Some((name, version.min(1)));
        }
    }
}

impl Dispatch<WlBuffer, Arc<AtomicBool>> for Globals {
    fn event(
        _: &mut Self,
        _: &WlBuffer,
        event: wl_buffer::Event,
        busy: &Arc<AtomicBool>,
        _: &Connection,
        _: &QueueHandle<Self>,
    ) {
        if let wl_buffer::Event::Release = event {
            busy.store(false, Ordering::Release);
        }
    }
}

delegate_noop!(Globals: ignore WlShm);
delegate_noop!(Globals: ignore WlShmPool);

// The program's own view of SDL's display: a queue of its own, so buffer
// events never reach SDL's dispatch, and the shared-memory global.
struct Session {
    connection: Connection,
    queue: EventQueue<Globals>,
    globals: Globals,
    shm: WlShm,
    _registry: WlRegistry,
}

impl Session {
    fn open() -> Option<Session> {
        // SAFETY: SDL's global properties are live while its video driver runs.
        let display = unsafe {
            SDL_GetPointerProperty(
                SDL_GetGlobalProperties(),
                c"SDL.video.wayland.wl_display".as_ptr(),
                std::ptr::null_mut(),
            )
        };
        if display.is_null() {
            return None;
        }
        // SAFETY: the display is SDL's live connection, which outlives this
        // session: `release_all` ends it before SDL quits.
        let backend = unsafe { Backend::from_foreign_display(display.cast()) };
        let connection = Connection::from_backend(backend);
        let mut queue = connection.new_event_queue();
        let handle = queue.handle();
        let registry = connection.display().get_registry(&handle, ());
        let mut globals = Globals::default();
        queue.roundtrip(&mut globals).ok()?;
        let (name, version) = globals.shm?;
        let shm = registry.bind(name, version, &handle, ());
        Some(Session {
            connection,
            queue,
            globals,
            shm,
            _registry: registry,
        })
    }

    // Takes in the buffer releases SDL's event pump has read. The socket is
    // SDL's to read: another reader blocks until every reader reads.
    fn settle(&mut self) {
        let _ = self.queue.dispatch_pending(&mut self.globals);
    }
}

// A frame-sized piece of shared memory mapped into the process.
struct Mapping {
    pixels: NonNull<u8>,
    len: usize,
}

impl Mapping {
    fn new(fd: impl AsFd, len: usize) -> Result<Mapping> {
        // SAFETY: a fresh shared mapping of `len` bytes of the memory file,
        // which was sized to `len` first.
        let pixels = unsafe {
            rustix::mm::mmap(
                std::ptr::null_mut(),
                len,
                rustix::mm::ProtFlags::READ | rustix::mm::ProtFlags::WRITE,
                rustix::mm::MapFlags::SHARED,
                fd,
                0,
            )
        }
        .map_err(|e| Error::new(format!("mapping a frame buffer: {e}")))?;
        let pixels = NonNull::new(pixels.cast::<u8>())
            .ok_or_else(|| Error::new("mapping a frame buffer answered null"))?;
        Ok(Mapping { pixels, len })
    }

    fn bytes(&self) -> &[u8] {
        // SAFETY: the mapping holds `len` bytes until it is dropped.
        unsafe { std::slice::from_raw_parts(self.pixels.as_ptr(), self.len) }
    }

    fn bytes_mut(&mut self) -> &mut [u8] {
        // SAFETY: the mapping holds `len` bytes until it is dropped, and
        // `&mut self` makes this the only view of them in the process.
        unsafe { std::slice::from_raw_parts_mut(self.pixels.as_ptr(), self.len) }
    }
}

impl Drop for Mapping {
    fn drop(&mut self) {
        // SAFETY: the mapping was made by `new` and nothing borrows it now.
        let _ = unsafe { rustix::mm::munmap(self.pixels.as_ptr().cast(), self.len) };
    }
}

struct Buffer {
    pixels: Mapping,
    pool: WlShmPool,
    buffer: WlBuffer,
    // Attached and not yet released by the compositor.
    busy: Arc<AtomicBool>,
    // The frame the buffer holds; a new buffer holds none.
    frame: Option<u64>,
}

impl Drop for Buffer {
    // The compositor keeps its own mapping of the memory, and this process
    // writes the buffer no more, so a buffer still shown stays intact.
    fn drop(&mut self) {
        self.buffer.destroy();
        self.pool.destroy();
    }
}

struct ShmSurface {
    surface: WlSurface,
    width: i64,
    height: i64,
    format: PixelFormat,
    buffers: Vec<Buffer>,
    // The buffer last attached.
    front: Option<usize>,
    // The number of the shown frame.
    frame: u64,
    // What each of the latest frames changed, by frame number, so a buffer
    // holding an earlier frame catches up by copying only those rectangles.
    history: VecDeque<(u64, Vec<Rect>)>,
}

thread_local! {
    static SESSION: RefCell<Option<Option<Session>>> = const { RefCell::new(None) };
    static SURFACES: RefCell<HashMap<u32, ShmSurface>> = RefCell::new(HashMap::new());
}

/// Whether the window's software surface is shared memory this module keeps.
pub(crate) fn applies() -> bool {
    crate::video_driver() == "wayland"
        && SESSION.with_borrow_mut(|session| session.get_or_insert_with(Session::open).is_some())
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

/// The byte order of the window's buffers: with coverage when the window is
/// see-through, else opaque.
pub(crate) fn format(window: &Window) -> PixelFormat {
    // SAFETY: the handle is a live window on the main thread.
    let flags = unsafe { SDL_GetWindowFlags(window.handle) };
    if flags & SDL_WINDOW_TRANSPARENT == SDL_WINDOW_TRANSPARENT {
        PixelFormat::Bgra8
    } else {
        PixelFormat::Bgrx8
    }
}

/// Paints into the buffer the window shows next, which first receives what
/// the shown frame changed, then attaches it. `damage` empty repaints every
/// pixel.
pub(crate) fn paint(
    window: &Window,
    damage: &[Rect],
    paint: impl FnOnce(&mut [u8], i64, i64, i64, PixelFormat),
) -> Result<()> {
    let (width, height) = pixel_size(window);
    if width <= 0 || height <= 0 {
        return Ok(());
    }
    let format = format(window);
    let damage: Vec<Rect> = if damage.is_empty() {
        vec![whole(width, height)]
    } else {
        damage
            .iter()
            .filter_map(|r| clip(*r, width, height))
            .collect()
    };
    SESSION.with_borrow_mut(|session| {
        let Some(Some(session)) = session.as_mut() else {
            return Err(Error::new("the Wayland shared-memory session is closed"));
        };
        session.settle();
        SURFACES.with_borrow_mut(|surfaces| {
            let id = window.id.0;
            let fits = surfaces
                .get(&id)
                .is_some_and(|s| s.width == width && s.height == height && s.format == format);
            if !fits {
                surfaces.remove(&id);
                let surface = surface_proxy(session, window)?;
                surfaces.insert(id, ShmSurface::new(surface, width, height, format));
            }
            let Some(surface) = surfaces.get_mut(&id) else {
                return Err(Error::new("the window's shared-memory surface is missing"));
            };
            surface.paint(session, &damage, paint)
        })?;
        // A full socket keeps the requests queued; SDL flushes the display
        // before it waits.
        let _ = session.connection.flush();
        Ok(())
    })
}

/// A copy of the pixels the window shows, `stride` bytes to a row.
pub(crate) struct Captured {
    pub(crate) pixels: Vec<u8>,
    pub(crate) width: i64,
    pub(crate) height: i64,
    pub(crate) stride: i64,
}

/// What the window shows, or `None` before the first paint.
pub(crate) fn capture(window: &Window) -> Option<Captured> {
    SURFACES.with_borrow(|surfaces| {
        let surface = surfaces.get(&window.id.0)?;
        let shown = &surface.buffers[surface.front?];
        Some(Captured {
            pixels: shown.pixels.bytes().to_vec(),
            width: surface.width,
            height: surface.height,
            stride: surface.width * 4,
        })
    })
}

/// Lets the window's buffers go, as it is hidden, minimized or closed, or a
/// GPU compositor takes over its drawing.
pub(crate) fn release(window: &Window) {
    SURFACES.with_borrow_mut(|surfaces| surfaces.remove(&window.id.0));
}

/// Lets every window's buffers and the session go as SDL quits.
pub(crate) fn release_all() {
    SURFACES.with_borrow_mut(std::mem::take);
    SESSION.with_borrow_mut(|session| *session = None);
}

// SDL's `wl_surface` for the window, as a proxy on the session's connection.
fn surface_proxy(session: &Session, window: &Window) -> Result<WlSurface> {
    // SAFETY: the handle is a live window on the main thread; SDL answers
    // null when the property is unset.
    let pointer = unsafe {
        SDL_GetPointerProperty(
            SDL_GetWindowProperties(window.handle),
            SDL_PROP_WINDOW_WAYLAND_SURFACE_POINTER,
            std::ptr::null_mut(),
        )
    };
    if pointer.is_null() {
        return Err(Error::new("the window has no Wayland surface"));
    }
    // SAFETY: the pointer is SDL's live wl_surface proxy on this display,
    // which SDL keeps until the window is destroyed; `release` drops this
    // proxy first.
    let id = unsafe { ObjectId::from_ptr(WlSurface::interface(), pointer.cast()) }
        .map_err(|e| Error::new(format!("reading the window's Wayland surface: {e}")))?;
    WlSurface::from_id(&session.connection, id)
        .map_err(|e| Error::new(format!("reading the window's Wayland surface: {e}")))
}

impl ShmSurface {
    fn new(surface: WlSurface, width: i64, height: i64, format: PixelFormat) -> ShmSurface {
        ShmSurface {
            surface,
            width,
            height,
            format,
            buffers: Vec::new(),
            front: None,
            frame: 0,
            history: VecDeque::new(),
        }
    }

    // A buffer to paint: the shown one once the compositor released it, as
    // it holds every pixel already; else one the compositor is not reading;
    // a new one while the pool has room; else the one with the oldest frame.
    // Waiting for a release could stall on a compositor that holds buffers
    // of a window it does not draw, so the pool's last buffer is reused.
    fn target(&mut self, session: &Session) -> Result<usize> {
        let free = |i: &usize| !self.buffers[*i].busy.load(Ordering::Acquire);
        if let Some(front) = self.front.filter(free) {
            return Ok(front);
        }
        let front = self.front;
        if let Some(i) = (0..self.buffers.len())
            .filter(|&i| Some(i) != front)
            .find(free)
        {
            return Ok(i);
        }
        if self.buffers.len() < POOL {
            self.buffers
                .push(buffer(session, self.width, self.height, self.format)?);
            return Ok(self.buffers.len() - 1);
        }
        (0..self.buffers.len())
            .filter(|&i| Some(i) != front)
            .min_by_key(|&i| self.buffers[i].frame)
            .ok_or_else(|| Error::new("the window has no buffer to paint"))
    }

    // What buffer `target` lacks of the shown frame: the rectangles every
    // later frame changed, or all of it when the history no longer reaches
    // back to the frame it holds.
    fn missing(&self, target: usize) -> Vec<Rect> {
        let whole = whole(self.width, self.height);
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
        session: &Session,
        damage: &[Rect],
        paint: impl FnOnce(&mut [u8], i64, i64, i64, PixelFormat),
    ) -> Result<()> {
        let back = self.target(session)?;
        let stride = self.width * 4;
        if let Some(front) = self.front.filter(|&front| front != back) {
            let missing = self.missing(back);
            let (from, to) = pair(&mut self.buffers, front, back);
            let (source, target) = (from.pixels.bytes(), to.pixels.bytes_mut());
            for r in missing {
                for row in r.y..r.y + r.height {
                    let at = (row * stride + r.x * 4) as usize;
                    let len = (r.width * 4) as usize;
                    target[at..at + len].copy_from_slice(&source[at..at + len]);
                }
            }
        }
        paint(
            self.buffers[back].pixels.bytes_mut(),
            self.width,
            self.height,
            stride,
            self.format,
        );
        self.frame += 1;
        self.buffers[back].frame = Some(self.frame);
        self.history.push_back((self.frame, damage.to_vec()));
        if self.history.len() > POOL {
            self.history.pop_front();
        }
        let shown = &self.buffers[back];
        shown.busy.store(true, Ordering::Release);
        self.surface.attach(Some(&shown.buffer), 0, 0);
        if self.surface.version() >= DAMAGE_BUFFER_VERSION {
            for r in damage {
                self.surface
                    .damage_buffer(r.x as i32, r.y as i32, r.width as i32, r.height as i32);
            }
        } else {
            // Surface coordinates differ from buffer pixels under scaling.
            self.surface.damage(0, 0, i32::MAX, i32::MAX);
        }
        self.surface.commit();
        self.front = Some(back);
        Ok(())
    }
}

// Buffer `a` to read and buffer `b` to write; they differ.
fn pair(buffers: &mut [Buffer], a: usize, b: usize) -> (&Buffer, &mut Buffer) {
    if a < b {
        let (low, high) = buffers.split_at_mut(b);
        (&low[a], &mut high[0])
    } else {
        let (low, high) = buffers.split_at_mut(a);
        (&high[0], &mut low[b])
    }
}

// A shared-memory buffer of `width` x `height` pixels in `format`.
fn buffer(session: &Session, width: i64, height: i64, format: PixelFormat) -> Result<Buffer> {
    let stride = width * 4;
    let len = stride
        .checked_mul(height)
        .and_then(|len| i32::try_from(len).ok())
        .ok_or_else(|| Error::new(format!("a {width}x{height} frame is too large to share")))?;
    let fd = rustix::fs::memfd_create(c"gui-frame", rustix::fs::MemfdFlags::CLOEXEC)
        .map_err(|e| Error::new(format!("making a frame buffer: {e}")))?;
    rustix::fs::ftruncate(&fd, len as u64)
        .map_err(|e| Error::new(format!("sizing a frame buffer: {e}")))?;
    let pixels = Mapping::new(&fd, len as usize)?;
    let handle = session.queue.handle();
    let pool = session.shm.create_pool(fd.as_fd(), len, &handle, ());
    let busy = Arc::new(AtomicBool::new(false));
    let shm_format = match format {
        PixelFormat::Bgra8 => wl_shm::Format::Argb8888,
        _ => wl_shm::Format::Xrgb8888,
    };
    let buffer = pool.create_buffer(
        0,
        width as i32,
        height as i32,
        stride as i32,
        shm_format,
        &handle,
        busy.clone(),
    );
    Ok(Buffer {
        pixels,
        pool,
        buffer,
        busy,
        frame: None,
    })
}

fn whole(width: i64, height: i64) -> Rect {
    Rect {
        x: 0,
        y: 0,
        width,
        height,
    }
}

// `r` within a `width` x `height` frame, or `None` when none of it is.
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
