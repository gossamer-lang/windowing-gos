//! Accelerated composition of software-rasterized RGBA layers through SDL.
//! The owner must drop this object before closing its window or SDL session.
use std::ffi::CStr;
use std::marker::PhantomData;
use std::ptr;
use std::rc::Rc;

use crate::{Error, Result, Window, display::Rect, support};
use sdl3_sys::blendmode::{SDL_BLENDMODE_BLEND, SDL_BLENDMODE_NONE};
use sdl3_sys::pixels::SDL_PIXELFORMAT_RGBA32;
use sdl3_sys::rect::SDL_Rect;
use sdl3_sys::render::*;
use sdl3_sys::surface::{SDL_ConvertSurface, SDL_DestroySurface};

/// A GPU compositor and persistent RGBA texture, confined to the UI thread.
/// GPU drawing never changes the software reference's color or sampling rules.
pub struct Compositor {
    renderer: *mut SDL_Renderer,
    texture: *mut SDL_Texture,
    width: i64,
    height: i64,
    name: String,
    cache: Vec<CachedLayer>,
    clock: u64,
    cache_bytes: usize,
    _thread: PhantomData<Rc<()>>,
}
struct CachedLayer {
    revision: u64,
    rect: Rect,
    texture: *mut SDL_Texture,
    bytes: usize,
    used: u64,
}

impl Compositor {
    pub fn new(window: Window, width: i64, height: i64) -> Result<Self> {
        support::require_main_thread("Compositor::new");
        if crate::video_driver() == "offscreen" {
            return Err(Error::new(
                "the offscreen EGL pbuffer does not resize; use the software compositor or a native display driver",
            ));
        }
        if width <= 0 || height <= 0 || width > (i32::MAX / 4) as i64 || height > i32::MAX as i64 {
            return Err(Error::new("invalid GPU surface size"));
        }
        // A software renderer is deliberately excluded: failing these drivers
        // leaves the caller free to use its retained software surface instead.
        // SAFETY: window is live on the UI thread and driver names are static.
        unsafe {
            sdl3_sys::video::SDL_DestroyWindowSurface(window.handle);
        }
        #[cfg(target_os = "macos")]
        crate::macos::layer::release(&window);
        let renderer = unsafe {
            SDL_CreateRenderer(
                window.handle,
                c"direct3d12,direct3d11,metal,opengl,opengles2,vulkan".as_ptr(),
            )
        };
        if renderer.is_null() {
            return Err(support::failure("creating the GPU compositor"));
        }
        let mut out = Self {
            renderer,
            texture: ptr::null_mut(),
            width,
            height,
            name: String::new(),
            cache: Vec::new(),
            clock: 0,
            cache_bytes: 0,
            _thread: PhantomData,
        };
        // SAFETY: the renderer is live; SDL owns its name until destruction.
        let name = unsafe { SDL_GetRendererName(renderer) };
        if !name.is_null() {
            out.name = unsafe { CStr::from_ptr(name) }
                .to_string_lossy()
                .into_owned();
        }
        // SAFETY: dimensions are positive and fit SDL's integer representation.
        out.texture = unsafe {
            SDL_CreateTexture(
                renderer,
                SDL_PIXELFORMAT_RGBA32,
                SDL_TEXTUREACCESS_TARGET,
                width as i32,
                height as i32,
            )
        };
        if out.texture.is_null() {
            return Err(support::failure("creating the GPU texture"));
        }
        // SAFETY: texture is live and contains an opaque final raster.
        support::check(
            unsafe { SDL_SetTextureBlendMode(out.texture, SDL_BLENDMODE_NONE) },
            "setting GPU texture blend",
        )?;
        Ok(out)
    }
    pub fn name(&self) -> &str {
        &self.name
    }
    pub fn size(&self) -> (i64, i64) {
        (self.width, self.height)
    }

    /// Uploads only damaged rows, then composes the retained texture. The
    /// backbuffer is never assumed to survive a swap. Optional readback occurs
    /// before present, as SDL requires, and is intended for validation only.
    pub fn present(
        &mut self,
        rgba: &[u8],
        damage: &[Rect],
        readback: bool,
    ) -> Result<Option<Vec<u8>>> {
        support::require_main_thread("Compositor::present");
        let expected = (self.width as usize)
            .checked_mul(self.height as usize)
            .and_then(|n| n.checked_mul(4))
            .ok_or_else(|| Error::new("GPU surface is too large"))?;
        if rgba.len() != expected {
            return Err(Error::new("GPU raster size mismatch"));
        }
        for r in damage {
            if r.x < 0
                || r.y < 0
                || r.width <= 0
                || r.height <= 0
                || r.x + r.width > self.width
                || r.y + r.height > self.height
            {
                return Err(Error::new("GPU damage is outside the raster"));
            }
            let rect = SDL_Rect {
                x: r.x as i32,
                y: r.y as i32,
                w: r.width as i32,
                h: r.height as i32,
            };
            let at = ((r.y * self.width + r.x) * 4) as usize;
            // SAFETY: the clipped rectangle and full pitch describe rows of
            // rgba; SDL consumes those bytes synchronously while texture lives.
            support::check(
                unsafe {
                    SDL_UpdateTexture(
                        self.texture,
                        &rect,
                        rgba[at..].as_ptr().cast(),
                        (self.width * 4) as i32,
                    )
                },
                "uploading GPU damage",
            )?;
        }
        self.finish(readback)
    }

    /// Starts repainting one damaged rectangle of the persistent GPU target.
    pub fn begin_damage(&mut self, rect: Rect) -> Result<()> {
        support::require_main_thread("Compositor::begin_damage");
        self.clock += 1;
        // SAFETY: renderer and target texture are owned by this compositor.
        support::check(
            unsafe { SDL_SetRenderTarget(self.renderer, self.texture) },
            "selecting GPU layer target",
        )?;
        self.clip(rect)?;
        support::check(
            unsafe { SDL_SetRenderDrawBlendMode(self.renderer, SDL_BLENDMODE_NONE) },
            "setting GPU clear blend",
        )?;
        self.fill(rect, [0, 0, 0, 0], false)
    }
    pub fn clip(&mut self, rect: Rect) -> Result<()> {
        support::require_main_thread("Compositor::clip");
        let r = SDL_Rect {
            x: rect.x as i32,
            y: rect.y as i32,
            w: rect.width as i32,
            h: rect.height as i32,
        };
        // SAFETY: SDL copies this rectangle during the call.
        support::check(
            unsafe { SDL_SetRenderClipRect(self.renderer, &r) },
            "clipping GPU commands",
        )
    }
    pub fn fill(&mut self, rect: Rect, color: [u8; 4], blend: bool) -> Result<()> {
        support::require_main_thread("Compositor::fill");
        let r = sdl3_sys::rect::SDL_FRect {
            x: rect.x as f32,
            y: rect.y as f32,
            w: rect.width as f32,
            h: rect.height as f32,
        };
        // SAFETY: renderer is live, and the rectangle is copied synchronously.
        unsafe {
            support::check(
                SDL_SetRenderDrawBlendMode(
                    self.renderer,
                    if blend {
                        SDL_BLENDMODE_BLEND
                    } else {
                        SDL_BLENDMODE_NONE
                    },
                ),
                "setting GPU fill blend",
            )?;
            support::check(
                SDL_SetRenderDrawColor(self.renderer, color[0], color[1], color[2], color[3]),
                "setting GPU fill color",
            )?;
            support::check(
                SDL_RenderFillRect(self.renderer, &r),
                "drawing GPU rectangle",
            )
        }
    }
    /// Lazily uploads a primitive layer. The cache owns at most `budget` bytes;
    /// an oversized layer is used transiently and immediately released.
    pub fn layer(
        &mut self,
        revision: u64,
        rect: Rect,
        budget: usize,
        make: impl FnOnce() -> Vec<u8>,
    ) -> Result<()> {
        support::require_main_thread("Compositor::layer");
        let index = self
            .cache
            .iter()
            .position(|l| l.revision == revision && l.rect == rect);
        let mut transient = false;
        let texture = if let Some(index) = index {
            self.cache[index].used = self.clock;
            self.cache[index].texture
        } else {
            let rgba = make();
            let expected = (rect.width as usize)
                .checked_mul(rect.height as usize)
                .and_then(|v| v.checked_mul(4))
                .ok_or_else(|| Error::new("GPU layer is too large"))?;
            if rgba.len() != expected {
                return Err(Error::new("invalid GPU layer pixels"));
            }
            // SAFETY: all dimensions describe the supplied RGBA allocation.
            let texture = unsafe {
                SDL_CreateTexture(
                    self.renderer,
                    SDL_PIXELFORMAT_RGBA32,
                    SDL_TEXTUREACCESS_STATIC,
                    rect.width as i32,
                    rect.height as i32,
                )
            };
            if texture.is_null() {
                return Err(support::failure("creating GPU primitive layer"));
            }
            let result = unsafe {
                support::check(
                    SDL_UpdateTexture(
                        texture,
                        ptr::null(),
                        rgba.as_ptr().cast(),
                        (rect.width * 4) as i32,
                    ),
                    "uploading GPU primitive",
                )
                .and_then(|()| {
                    support::check(
                        SDL_SetTextureBlendMode(texture, SDL_BLENDMODE_BLEND),
                        "setting GPU layer blend",
                    )
                })
            };
            if let Err(error) = result {
                unsafe { SDL_DestroyTexture(texture) };
                return Err(error);
            }
            while self.cache_bytes.saturating_add(expected) > budget && !self.cache.is_empty() {
                let i = self
                    .cache
                    .iter()
                    .enumerate()
                    .min_by_key(|(_, l)| l.used)
                    .unwrap()
                    .0;
                let layer = self.cache.remove(i);
                self.cache_bytes -= layer.bytes;
                unsafe { SDL_DestroyTexture(layer.texture) };
            }
            if expected <= budget {
                self.cache.push(CachedLayer {
                    revision,
                    rect,
                    texture,
                    bytes: expected,
                    used: self.clock,
                });
                self.cache_bytes += expected;
            } else {
                transient = true;
            }
            texture
        };
        let dest = sdl3_sys::rect::SDL_FRect {
            x: rect.x as f32,
            y: rect.y as f32,
            w: rect.width as f32,
            h: rect.height as f32,
        };
        // SAFETY: texture and renderer are live and the destination is copied.
        let result = support::check(
            unsafe { SDL_RenderTexture(self.renderer, texture, ptr::null(), &dest) },
            "composing GPU primitive",
        );
        if transient {
            unsafe { SDL_DestroyTexture(texture) };
        }
        result
    }
    pub fn finish(&mut self, readback: bool) -> Result<Option<Vec<u8>>> {
        support::require_main_thread("Compositor::finish");
        // The platform backbuffer is reconstructed after every swap; the
        // persistent target, rather than that backbuffer, holds undamaged pixels.
        unsafe {
            support::check(
                SDL_SetRenderTarget(self.renderer, ptr::null_mut()),
                "selecting window target",
            )?;
            support::check(
                SDL_SetRenderClipRect(self.renderer, ptr::null()),
                "resetting GPU clip",
            )?;
            support::check(
                SDL_RenderTexture(self.renderer, self.texture, ptr::null(), ptr::null()),
                "composing GPU frame",
            )?;
        }
        let pixels = if readback {
            Some(self.readback()?)
        } else {
            None
        };
        support::check(
            unsafe { SDL_RenderPresent(self.renderer) },
            "presenting GPU frame",
        )?;
        Ok(pixels)
    }
    pub fn trim_layers(&mut self, budget: usize) {
        support::require_main_thread("Compositor::trim_layers");
        while self.cache_bytes > budget {
            let index = self
                .cache
                .iter()
                .enumerate()
                .min_by_key(|(_, l)| l.used)
                .unwrap()
                .0;
            let layer = self.cache.remove(index);
            self.cache_bytes -= layer.bytes;
            // SAFETY: this cache exclusively owns the texture on the UI thread.
            unsafe { SDL_DestroyTexture(layer.texture) };
        }
    }
    pub fn cache_bytes(&self) -> usize {
        self.cache_bytes
    }

    fn readback(&self) -> Result<Vec<u8>> {
        // SAFETY: renderer is live; readback is before present. Each owned
        // surface is released, including on conversion failure.
        unsafe {
            let surface = SDL_RenderReadPixels(self.renderer, ptr::null());
            if surface.is_null() {
                return Err(support::failure("reading GPU pixels"));
            }
            let rgba = SDL_ConvertSurface(surface, SDL_PIXELFORMAT_RGBA32);
            SDL_DestroySurface(surface);
            if rgba.is_null() {
                return Err(support::failure("converting GPU readback"));
            }
            let width = (*rgba).w as usize;
            let height = (*rgba).h as usize;
            let pitch = (*rgba).pitch as usize;
            let mut pixels = Vec::with_capacity(width * height * 4);
            for y in 0..height {
                pixels.extend_from_slice(std::slice::from_raw_parts(
                    (*rgba).pixels.cast::<u8>().add(y * pitch),
                    width * 4,
                ));
            }
            SDL_DestroySurface(rgba);
            Ok(pixels)
        }
    }
}
impl Drop for Compositor {
    fn drop(&mut self) {
        support::require_main_thread("Compositor::drop");
        // SAFETY: this object exclusively owns the texture and renderer, and
        // the toolkit releases it before closing the native window.
        unsafe {
            for layer in self.cache.drain(..) {
                SDL_DestroyTexture(layer.texture);
            }
            if !self.texture.is_null() {
                SDL_DestroyTexture(self.texture);
            }
            SDL_DestroyRenderer(self.renderer);
        }
    }
}
