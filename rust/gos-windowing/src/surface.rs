//! What a program draws into: the software framebuffer, the raw native
//! handles a GPU API binds to, and the OpenGL, Vulkan, and Metal helpers.

use std::ffi::{CString, c_void};

use sdl3_sys::metal::{
    SDL_Metal_CreateView, SDL_Metal_DestroyView, SDL_Metal_GetLayer, SDL_MetalView,
};
use sdl3_sys::pixels::{
    SDL_PIXELFORMAT_ABGR8888, SDL_PIXELFORMAT_ARGB8888, SDL_PIXELFORMAT_XBGR8888,
    SDL_PIXELFORMAT_XRGB8888,
};
use sdl3_sys::properties::{SDL_GetNumberProperty, SDL_GetPointerProperty};
use sdl3_sys::rect::SDL_Rect;
use sdl3_sys::surface::{SDL_CreateSurface, SDL_Surface};
use sdl3_sys::video::*;
use sdl3_sys::vulkan::{SDL_Vulkan_CreateSurface, SDL_Vulkan_GetInstanceExtensions};

use crate::window::{Window, WindowId};
use crate::{Error, Result, support};

/// The byte order of a frame's pixels: four bytes each.
#[derive(Debug, Clone, Copy, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
pub enum PixelFormat {
    Bgra8,
    Rgba8,
    Bgrx8,
    Rgbx8,
}

impl PixelFormat {
    /// Byte offsets of red, green, and blue within one pixel.
    pub fn rgb_offsets(&self) -> (usize, usize, usize) {
        match self {
            PixelFormat::Bgra8 | PixelFormat::Bgrx8 => (2, 1, 0),
            PixelFormat::Rgba8 | PixelFormat::Rgbx8 => (0, 1, 2),
        }
    }
}

/// A software framebuffer for one window: `height` rows of `stride` bytes,
/// each pixel four bytes in `format`'s order.
#[derive(Debug, Clone, PartialEq)]
pub struct Frame {
    pub pixels: Vec<u8>,
    pub width: i64,
    pub height: i64,
    pub stride: i64,
    pub format: PixelFormat,
    pub window: WindowId,
}

impl Frame {
    /// Sets one pixel, opaque. Panics outside the frame.
    pub fn set_pixel(&mut self, x: i64, y: i64, r: u8, g: u8, b: u8) {
        if x < 0 || x >= self.width || y < 0 || y >= self.height {
            panic!(
                "the pixel ({x}, {y}) lies outside a {}x{} frame",
                self.width, self.height
            );
        }
        let at = (y * self.stride + x * 4) as usize;
        let (ro, go, bo) = self.format.rgb_offsets();
        self.pixels[at + ro] = r;
        self.pixels[at + go] = g;
        self.pixels[at + bo] = b;
        self.pixels[at + 3] = 255;
    }

    /// Fills every pixel with one opaque color, leaving row padding alone.
    pub fn fill(&mut self, r: u8, g: u8, b: u8) {
        let (ro, go, bo) = self.format.rgb_offsets();
        let mut pixel = [0u8, 0, 0, 255];
        pixel[ro] = r;
        pixel[go] = g;
        pixel[bo] = b;
        let row_bytes = (self.width * 4) as usize;
        for row in self
            .pixels
            .chunks_mut(self.stride as usize)
            .take(self.height as usize)
        {
            for px in row[..row_bytes].as_chunks_mut::<4>().0 {
                *px = pixel;
            }
        }
    }
}

/// The native handles of a window, for a GPU API binding.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum RawHandle {
    Win32 {
        hwnd: *mut c_void,
        hinstance: *mut c_void,
    },
    AppKit {
        ns_window: *mut c_void,
        ns_view: *mut c_void,
    },
    Wayland {
        display: *mut c_void,
        surface: *mut c_void,
    },
    Xlib {
        display: *mut c_void,
        window: u64,
        screen: i64,
    },
    Drm {
        fd: i64,
        gbm_device: *mut c_void,
    },
}

/// An OpenGL profile.
#[derive(Debug, Clone, Copy, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
pub enum GlProfile {
    Core,
    Compatibility,
    Es,
}

/// What an OpenGL context is created with.
#[derive(Debug, Clone, Copy, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
pub struct GlOptions {
    pub major: i64,
    pub minor: i64,
    pub profile: GlProfile,
    pub depth_bits: i64,
    pub stencil_bits: i64,
    pub samples: i64,
    pub srgb: bool,
    pub debug: bool,
}

impl Default for GlOptions {
    fn default() -> GlOptions {
        GlOptions {
            major: 3,
            minor: 3,
            profile: GlProfile::Core,
            depth_bits: 24,
            stencil_bits: 8,
            samples: 0,
            srgb: false,
            debug: false,
        }
    }
}

/// An OpenGL context bound to one window.
#[derive(Debug)]
pub struct GlContext {
    context: SDL_GLContext,
    window: *mut SDL_Window,
}

impl GlContext {
    pub fn make_current(&self) -> Result<()> {
        // SAFETY: window and context are live.
        support::check(
            unsafe { SDL_GL_MakeCurrent(self.window, self.context) },
            "making the GL context current",
        )
    }

    pub fn swap(&self) -> Result<()> {
        // SAFETY: window is live.
        support::check(
            unsafe { SDL_GL_SwapWindow(self.window) },
            "swapping GL buffers",
        )
    }

    pub fn set_swap_interval(&self, interval: i64) -> Result<()> {
        let value = support::c_int(interval, "a swap interval")?;
        // SAFETY: applies to the current context.
        support::check(
            unsafe { SDL_GL_SetSwapInterval(value) },
            "setting the swap interval",
        )
    }

    /// The address of the GL function `name`, or null.
    pub fn proc_address(&self, name: &str) -> *const c_void {
        let Ok(text) = CString::new(name) else {
            return std::ptr::null();
        };
        // SAFETY: text outlives the call.
        match unsafe { SDL_GL_GetProcAddress(text.as_ptr()) } {
            Some(f) => f as *const c_void,
            None => std::ptr::null(),
        }
    }

    pub fn destroy(&self) -> Result<()> {
        // SAFETY: the context is live and not used after this.
        support::check(
            unsafe { SDL_GL_DestroyContext(self.context) },
            "destroying the GL context",
        )
    }
}

/// A Metal view and its layer (macOS).
#[derive(Debug)]
pub struct MetalView {
    pub view: SDL_MetalView,
    pub layer: *mut c_void,
}

impl MetalView {
    pub fn destroy(&self) {
        // SAFETY: the view is live and not used after this.
        unsafe { SDL_Metal_DestroyView(self.view) }
    }
}

/// The window's software surface, as SDL describes it.
pub(crate) struct SurfaceInfo {
    pub(crate) pixels: *mut u8,
    pub(crate) width: i64,
    pub(crate) height: i64,
    pub(crate) stride: i64,
    pub(crate) format: PixelFormat,
}

impl Window {
    /// A zeroed frame matching the window's surface.
    pub fn framebuffer(&self) -> Result<Frame> {
        support::require_main_thread("Window::framebuffer");
        let surface = self.window_surface()?;
        Ok(Frame {
            pixels: vec![0u8; (surface.stride * surface.height) as usize],
            width: surface.width,
            height: surface.height,
            stride: surface.stride,
            format: surface.format,
            window: self.id,
        })
    }

    /// Shows `frame`: one copy into the surface the window system displays.
    pub fn present(&self, frame: &Frame) -> Result<()> {
        support::require_main_thread("Window::present");
        let surface = self.window_surface()?;
        if surface.width != frame.width
            || surface.height != frame.height
            || surface.stride != frame.stride
        {
            return Err(Error::new(format!(
                "the frame is {}x{} but the window is now {}x{}; take a new framebuffer after a resize",
                frame.width, frame.height, surface.width, surface.height
            )));
        }
        // SAFETY: the surface holds stride * height bytes, the frame's length.
        unsafe {
            std::ptr::copy_nonoverlapping(frame.pixels.as_ptr(), surface.pixels, frame.pixels.len())
        };
        // SAFETY: handle is live.
        support::check(
            unsafe { SDL_UpdateWindowSurface(self.handle) },
            "presenting the frame",
        )
    }

    /// Paints straight into the window's surface: `paint` gets its bytes,
    /// width, height, stride and format; then the rectangles `damage` names
    /// (every pixel when empty) are shown.
    pub fn paint_surface(
        &self,
        damage: &[crate::display::Rect],
        paint: impl FnOnce(&mut [u8], i64, i64, i64, PixelFormat),
    ) -> Result<()> {
        support::require_main_thread("Window::paint_surface");
        let surface = self.window_surface()?;
        // SAFETY: the surface holds stride * height bytes until the next resize,
        // and nothing else touches it on this thread meanwhile.
        let bytes = unsafe {
            std::slice::from_raw_parts_mut(
                surface.pixels,
                (surface.stride * surface.height) as usize,
            )
        };
        paint(
            bytes,
            surface.width,
            surface.height,
            surface.stride,
            surface.format,
        );
        if damage.is_empty() {
            // SAFETY: handle is live.
            return support::check(
                unsafe { SDL_UpdateWindowSurface(self.handle) },
                "presenting the frame",
            );
        }
        let rects: Vec<SDL_Rect> = damage
            .iter()
            .map(|r| SDL_Rect {
                x: r.x as i32,
                y: r.y as i32,
                w: r.width as i32,
                h: r.height as i32,
            })
            .collect();
        support::check(
            // SAFETY: rects is a valid array of len entries.
            unsafe {
                SDL_UpdateWindowSurfaceRects(self.handle, rects.as_ptr(), rects.len() as i32)
            },
            "presenting the frame",
        )
    }

    /// What the window shows now: a copy of its software surface as a frame.
    pub fn capture(&self) -> Result<Frame> {
        support::require_main_thread("Window::capture");
        let mut frame = self.framebuffer()?;
        let surface = self.window_surface()?;
        // SAFETY: the surface holds stride * height bytes.
        frame.pixels = unsafe {
            std::slice::from_raw_parts(surface.pixels, (frame.stride * frame.height) as usize)
        }
        .to_vec();
        Ok(frame)
    }

    /// The window's native handles, for a GPU API binding.
    pub fn raw_handle(&self) -> Result<RawHandle> {
        support::require_main_thread("Window::raw_handle");
        // SAFETY: handle is live.
        let props = unsafe { SDL_GetWindowProperties(self.handle) };
        // SAFETY: props is live; SDL answers null when unset.
        let pointer = |name| unsafe { SDL_GetPointerProperty(props, name, std::ptr::null_mut()) };
        // SAFETY: props is live; SDL answers 0 when unset.
        let number = |name| unsafe { SDL_GetNumberProperty(props, name, 0) };
        let driver = crate::video_driver();
        let missing = || {
            Error::new(format!(
                "the `{driver}` video driver gave this window no native handle"
            ))
        };
        let present = |p: *mut c_void| if p.is_null() { Err(missing()) } else { Ok(p) };
        match driver.as_str() {
            "windows" => Ok(RawHandle::Win32 {
                hwnd: present(pointer(SDL_PROP_WINDOW_WIN32_HWND_POINTER))?,
                hinstance: pointer(SDL_PROP_WINDOW_WIN32_INSTANCE_POINTER),
            }),
            "cocoa" => {
                let ns_window = present(pointer(SDL_PROP_WINDOW_COCOA_WINDOW_POINTER))?;
                Ok(RawHandle::AppKit {
                    ns_window,
                    ns_view: crate::macos::content_view(ns_window),
                })
            }
            "wayland" => Ok(RawHandle::Wayland {
                display: present(pointer(SDL_PROP_WINDOW_WAYLAND_DISPLAY_POINTER))?,
                surface: present(pointer(SDL_PROP_WINDOW_WAYLAND_SURFACE_POINTER))?,
            }),
            "x11" => Ok(RawHandle::Xlib {
                display: present(pointer(SDL_PROP_WINDOW_X11_DISPLAY_POINTER))?,
                window: number(SDL_PROP_WINDOW_X11_WINDOW_NUMBER) as u64,
                screen: number(SDL_PROP_WINDOW_X11_SCREEN_NUMBER),
            }),
            "kmsdrm" => Ok(RawHandle::Drm {
                fd: number(SDL_PROP_WINDOW_KMSDRM_DRM_FD_NUMBER),
                gbm_device: pointer(SDL_PROP_WINDOW_KMSDRM_GBM_DEVICE_POINTER),
            }),
            _ => Err(missing()),
        }
    }

    /// An OpenGL context for a window created with `Graphics::OpenGl`.
    pub fn gl_context(&self, options: GlOptions) -> Result<GlContext> {
        support::require_main_thread("Window::gl_context");
        let profile_mask = match options.profile {
            GlProfile::Core => SDL_GL_CONTEXT_PROFILE_CORE,
            GlProfile::Compatibility => SDL_GL_CONTEXT_PROFILE_COMPATIBILITY,
            GlProfile::Es => SDL_GL_CONTEXT_PROFILE_ES,
        };
        let mut flags = 0;
        if options.debug {
            flags |= SDL_GL_CONTEXT_DEBUG_FLAG.0;
        }
        if options.profile == GlProfile::Core {
            flags |= SDL_GL_CONTEXT_FORWARD_COMPATIBLE_FLAG.0;
        }
        let attributes = [
            (
                SDL_GL_CONTEXT_MAJOR_VERSION,
                support::c_int(options.major, "a GL major version")?,
            ),
            (
                SDL_GL_CONTEXT_MINOR_VERSION,
                support::c_int(options.minor, "a GL minor version")?,
            ),
            (SDL_GL_CONTEXT_PROFILE_MASK, profile_mask.0),
            (SDL_GL_CONTEXT_FLAGS, flags),
            (SDL_GL_DOUBLEBUFFER, 1),
            (
                SDL_GL_DEPTH_SIZE,
                support::c_int(options.depth_bits, "a depth size")?,
            ),
            (
                SDL_GL_STENCIL_SIZE,
                support::c_int(options.stencil_bits, "a stencil size")?,
            ),
            (SDL_GL_MULTISAMPLEBUFFERS, i32::from(options.samples > 0)),
            (
                SDL_GL_MULTISAMPLESAMPLES,
                support::c_int(options.samples, "a sample count")?,
            ),
            (SDL_GL_FRAMEBUFFER_SRGB_CAPABLE, i32::from(options.srgb)),
        ];
        // SAFETY: resets global GL attribute state on the main thread.
        unsafe { SDL_GL_ResetAttributes() };
        for (attribute, value) in attributes {
            // SAFETY: attribute is a valid SDL_GLAttr.
            support::check(
                unsafe { SDL_GL_SetAttribute(attribute, value) },
                "setting a GL attribute",
            )?;
        }
        // SAFETY: handle is live.
        let context = unsafe { SDL_GL_CreateContext(self.handle) };
        if context.is_null() {
            return Err(support::failure("creating a GL context"));
        }
        Ok(GlContext {
            context,
            window: self.handle,
        })
    }

    /// A Vulkan surface for `instance` (a `VkInstance`), as its raw handle.
    pub fn vulkan_surface(&self, instance: *mut c_void) -> Result<u64> {
        support::require_main_thread("Window::vulkan_surface");
        let mut surface = 0u64;
        support::check(
            // SAFETY: instance is the caller's live VkInstance; surface is valid storage.
            unsafe {
                SDL_Vulkan_CreateSurface(
                    self.handle,
                    instance.cast(),
                    std::ptr::null(),
                    (&mut surface as *mut u64).cast(),
                )
            },
            "creating a Vulkan surface",
        )?;
        Ok(surface)
    }

    /// A Metal view and layer for a window created with `Graphics::Metal`.
    pub fn metal_view(&self) -> Result<MetalView> {
        support::require_main_thread("Window::metal_view");
        // SAFETY: handle is live.
        let view = unsafe { SDL_Metal_CreateView(self.handle) };
        if view.is_null() {
            return Err(support::failure("creating a Metal view"));
        }
        // SAFETY: view is live.
        let layer = unsafe { SDL_Metal_GetLayer(view) };
        if layer.is_null() {
            // SAFETY: view was created above and is not shared.
            unsafe { SDL_Metal_DestroyView(view) };
            return Err(support::failure("reading the Metal layer"));
        }
        Ok(MetalView { view, layer })
    }

    pub(crate) fn window_surface(&self) -> Result<SurfaceInfo> {
        // SAFETY: handle is live; SDL owns the surface it answers.
        let surface = unsafe { SDL_GetWindowSurface(self.handle) };
        if surface.is_null() {
            return Err(support::failure("reading the window surface"));
        }
        // SAFETY: surface is non-null and valid until the next resize.
        let s = unsafe { &*surface };
        let format = match s.format {
            SDL_PIXELFORMAT_ARGB8888 => PixelFormat::Bgra8,
            SDL_PIXELFORMAT_XRGB8888 => PixelFormat::Bgrx8,
            SDL_PIXELFORMAT_ABGR8888 => PixelFormat::Rgba8,
            SDL_PIXELFORMAT_XBGR8888 => PixelFormat::Rgbx8,
            other => {
                return Err(Error::new(format!(
                    "the window surface uses pixel format {}, which has no 32-bit RGB byte order",
                    other.0
                )));
            }
        };
        Ok(SurfaceInfo {
            pixels: s.pixels.cast::<u8>(),
            width: i64::from(s.w),
            height: i64::from(s.h),
            stride: i64::from(s.pitch),
            format,
        })
    }
}

/// The Vulkan instance extensions a window surface needs.
pub fn vulkan_instance_extensions() -> Result<Vec<String>> {
    support::require_main_thread("windowing::vulkan_instance_extensions");
    let mut count = 0u32;
    // SAFETY: count receives the list length; SDL owns the list.
    let names = unsafe { SDL_Vulkan_GetInstanceExtensions(&mut count) };
    if names.is_null() {
        return Err(support::failure("listing Vulkan instance extensions"));
    }
    // SAFETY: SDL answered `count` C strings it owns.
    Ok(unsafe { std::slice::from_raw_parts(names, count as usize) }
        .iter()
        .map(|p| support::read_text(*p))
        .collect())
}

/// A new SDL surface holding `width` x `height` RGBA pixels.
pub(crate) fn rgba_surface(rgba: &[u8], width: i64, height: i64) -> Result<*mut SDL_Surface> {
    if width <= 0 || height <= 0 {
        return Err(Error::new(format!(
            "an image needs a positive size, not {width}x{height}"
        )));
    }
    let expected = width * height * 4;
    if rgba.len() as i64 != expected {
        return Err(Error::new(format!(
            "an image of {width}x{height} pixels needs {expected} RGBA bytes, not {}",
            rgba.len()
        )));
    }
    let w = support::c_int(width, "an image width")?;
    let h = support::c_int(height, "an image height")?;
    // ABGR8888 is a packed 32-bit word whose bytes, on every little-endian
    // target, are R, G, B, A in memory.
    // SAFETY: plain allocation of a w x h surface.
    let surface = unsafe { SDL_CreateSurface(w, h, SDL_PIXELFORMAT_ABGR8888) };
    if surface.is_null() {
        return Err(support::failure("creating an image surface"));
    }
    // SAFETY: surface is non-null; its pitch * h bytes are writable.
    let info = unsafe { &*surface };
    let row = (width * 4) as usize;
    let pitch = info.pitch as usize;
    for (y, src) in rgba.chunks_exact(row).enumerate() {
        // SAFETY: each destination row starts inside the surface and holds `row` bytes.
        unsafe {
            std::ptr::copy_nonoverlapping(
                src.as_ptr(),
                info.pixels.cast::<u8>().add(y * pitch),
                row,
            );
        }
    }
    Ok(surface)
}

#[cfg(test)]
mod tests {
    use super::{Frame, PixelFormat};
    use crate::window::WindowId;

    // A 3x2 frame whose rows carry 4 bytes of padding past their pixels.
    fn padded(order: PixelFormat) -> Frame {
        Frame {
            pixels: vec![0u8; 32],
            width: 3,
            height: 2,
            stride: 16,
            format: order,
            window: WindowId(2),
        }
    }

    #[test]
    fn fill_paints_every_pixel_and_leaves_row_padding_alone() {
        let mut frame = padded(PixelFormat::Bgrx8);
        frame.fill(10, 20, 30);
        for y in 0..2 {
            let row = y * 16;
            for x in 0..3 {
                let at = row + x * 4;
                assert_eq!(frame.pixels[at..at + 4], [30u8, 20, 10, 255]);
            }
            assert_eq!(frame.pixels[row + 12..row + 16], [0u8, 0, 0, 0]);
        }
    }

    #[test]
    fn set_pixel_writes_one_pixel_in_the_frame_byte_order() {
        let mut frame = padded(PixelFormat::Rgba8);
        frame.set_pixel(2, 1, 1, 2, 3);
        let at = 16 + 2 * 4;
        assert_eq!(frame.pixels[at..at + 4], [1u8, 2, 3, 255]);
        assert_eq!(frame.pixels.iter().filter(|b| **b != 0).count(), 4);
    }

    #[test]
    fn rgb_offsets_follow_the_format_byte_order() {
        assert_eq!(PixelFormat::Bgra8.rgb_offsets(), (2, 1, 0));
        assert_eq!(PixelFormat::Bgrx8.rgb_offsets(), (2, 1, 0));
        assert_eq!(PixelFormat::Rgba8.rgb_offsets(), (0, 1, 2));
        assert_eq!(PixelFormat::Rgbx8.rgb_offsets(), (0, 1, 2));
    }
}
