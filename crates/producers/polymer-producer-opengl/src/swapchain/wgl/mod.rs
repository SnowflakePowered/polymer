mod native;
mod procedures;
mod read_binding;

use crate::interop::{Hdc, Hglrc, current_context};
use native::Pbuffer;
use parking_lot::Mutex;
use polymer_hook::config::{self, RenderMode};
use polymer_platform_windows::{FixedWindowBinding, NativeWindow, WindowLifetime};
use polymer_presentation::{producer::ProducerWindows, swapchain::*};
use std::{
    cell::Cell,
    collections::HashSet,
    sync::{Arc, LazyLock},
};

#[link(name = "opengl32")]
unsafe extern "system" {
    fn wglMakeCurrent(dc: Hdc, context: Hglrc) -> i32;
    fn wglGetCurrentDC() -> Hdc;
}
#[link(name = "user32")]
unsafe extern "system" {
    fn WindowFromDC(dc: Hdc) -> *mut std::ffi::c_void;
}
#[link(name = "gdi32")]
unsafe extern "system" {
    fn GetPixelFormat(dc: Hdc) -> i32;
}

thread_local! {
    static INTERNAL: Cell<bool> = const { Cell::new(false) };
    static BINDING: Cell<(usize, usize, usize)> = const { Cell::new((0, 0, 0)) };
}

struct NativeScope(bool);
impl Drop for NativeScope {
    fn drop(&mut self) {
        INTERNAL.set(self.0);
    }
}

struct Drawable {
    buffer: Pbuffer,
    binding: Arc<FixedSurfaceBinding>,
    _registration: SourceRegistration,
    _fixed: FixedWindowBinding,
    contexts: HashSet<usize>,
    initialize_window: bool,
    read: Option<read_binding::ReadFunctions>,
}

type DrawableKey = (isize, u64, i32);
static DRAWABLES: LazyLock<dashmap::DashMap<DrawableKey, Mutex<Drawable>>> =
    LazyLock::new(Default::default);
static CREATION: Mutex<()> = Mutex::new(());
static ASSIGNMENTS: LazyLock<Result<Arc<AssignmentRegistry>, String>> = LazyLock::new(|| {
    AssignmentRegistry::new(config::render_configuration().map_err(str::to_owned)?)
});

/// WGL routing is separate from OpenGL state: framebuffer zero stays native.
pub struct WglSwapchain;
impl WglSwapchain {
    pub fn enabled() -> bool {
        !INTERNAL.get() && config::render_mode() == Ok(RenderMode::Swapchain)
    }

    pub fn native<T>(operation: impl FnOnce() -> T) -> T {
        let _scope = NativeScope(INTERNAL.replace(true));
        operation()
    }

    /// # Safety
    /// The callback must call the original WGL binding entry point.
    pub unsafe fn make_current(
        draw: Hdc,
        read: Hdc,
        context: Hglrc,
        native: impl FnOnce(Hdc, Hdc, Hglrc) -> i32,
    ) -> i32 {
        if !Self::enabled() {
            return native(draw, read, context);
        }
        Self::native(|| {
            // A current context is needed to resolve ICD-specific extensions.
            // Existing drawables are routed before binding, so normal rebinds
            // never activate the resized window's framebuffer.
            let result = unsafe { Self::bind(draw, read, context, native) };
            if result != 0 {
                BINDING.set((draw as usize, read as usize, context as usize));
                unsafe { Self::initialize_window(draw) };
            } else {
                BINDING.set((0, 0, 0));
            }
            result
        })
    }

    unsafe fn bind(
        draw: Hdc,
        read: Hdc,
        context: Hglrc,
        native: impl FnOnce(Hdc, Hdc, Hglrc) -> i32,
    ) -> i32 {
        if context.is_null() {
            return native(draw, read, context);
        }
        let routed = unsafe { Self::prepare_binding(draw, read, context) };
        match routed {
            Ok((draw, read)) => native(draw, read, context),
            Err(error) => {
                tracing::error!(%error, "OpenGL pbuffer binding failed");
                unsafe { wglMakeCurrent(std::ptr::null_mut(), std::ptr::null_mut()) };
                0
            }
        }
    }

    unsafe fn route(dc: Hdc, context: Hglrc) -> Result<Hdc, String> {
        let hwnd = unsafe { WindowFromDC(dc) } as isize;
        if hwnd == 0 {
            return Ok(dc);
        }
        let window = NativeWindow(hwnd);
        let lifetime = WindowLifetime::identify(window).map_err(|error| error.to_string())?;
        let format = unsafe { GetPixelFormat(dc) };
        let key = (hwnd, lifetime, format);
        let _creation = CREATION.lock();
        if let Some(entry) = DRAWABLES.get(&key) {
            let mut drawable = entry.lock();
            if drawable.buffer.lost() {
                return Err(
                    "pbuffer lost after display-mode change; recreate the rendering context".into(),
                );
            }
            drawable.contexts.insert(context as usize);
            return Ok(drawable.buffer.dc as Hdc);
        }
        let registry = ASSIGNMENTS.as_ref().map_err(Clone::clone)?;
        let registration = registry
            .register(Candidate {
                source: SourceIdentity {
                    device: format as u64,
                    object: hwnd as u64,
                    lifetime,
                },
                window: WindowIdentity {
                    handle: hwnd as u64,
                    lifetime,
                },
                title: polymer_platform_windows::window_title(window).unwrap_or_default(),
            })
            .map_err(str::to_owned)?;
        let Some(reservation) = registration.reserve() else {
            return Ok(dc);
        };
        let binding = reservation.shared_binding();
        let buffer = unsafe { Pbuffer::new(dc, format, binding.extent()) }?;
        let fixed = ProducerWindows::bind_fixed(&binding).map_err(|error| error.to_string())?;
        let routed = buffer.dc as Hdc;
        reservation.commit().map_err(str::to_owned)?;
        tracing::info!(
            window = hwnd,
            dc = buffer.dc,
            width = binding.extent().width,
            height = binding.extent().height,
            "created fixed OpenGL pbuffer swapchain"
        );
        DRAWABLES.insert(
            key,
            Mutex::new(Drawable {
                buffer,
                binding,
                _registration: registration,
                _fixed: fixed,
                contexts: HashSet::from([context as usize]),
                initialize_window: true,
                read: unsafe { read_binding::ReadFunctions::load() },
            }),
        );
        Ok(routed)
    }

    unsafe fn initialize_window(dc: Hdc) {
        let hwnd = unsafe { WindowFromDC(dc) } as isize;
        let extent = DRAWABLES.iter().find_map(|entry| {
            let mut drawable = entry.lock();
            if entry.key().0 != hwnd || !drawable.initialize_window {
                return None;
            }
            drawable.initialize_window = false;
            Some(drawable.binding.extent())
        });
        if let Some(extent) = extent {
            // Initialize application-side size caches once, just as the DXGI
            // frontends do. No resize messages govern the pbuffer's lifetime.
            if let Err(error) = polymer_platform_windows::resize_client(
                NativeWindow(hwnd),
                extent.width,
                extent.height,
            ) {
                tracing::warn!(%error, "could not initialize OpenGL window dimensions");
            }
        }
    }

    unsafe fn prepare_binding(draw: Hdc, read: Hdc, context: Hglrc) -> Result<(Hdc, Hdc), String> {
        let known_draw = unsafe { Self::lookup(draw, context) };
        let known_read = unsafe { Self::lookup(read, context) };
        // Bootstrap against an existing pbuffer whenever possible, including
        // after unbinding or moving the context to another rendering thread.
        let bootstrap = known_draw.unwrap_or(draw);
        if unsafe { current_context() } != context
            && unsafe { wglMakeCurrent(bootstrap, context) } == 0
        {
            return Err("could not activate the application's OpenGL context".into());
        }
        let draw = match known_draw {
            Some(dc) => dc,
            None => unsafe { Self::route(draw, context) }?,
        };
        let read = match known_read {
            Some(dc) => dc,
            None => unsafe { Self::route(read, context) }?,
        };
        Ok((draw, read))
    }

    unsafe fn lookup(dc: Hdc, context: Hglrc) -> Option<Hdc> {
        let hwnd = unsafe { WindowFromDC(dc) } as isize;
        if hwnd == 0 {
            return Some(dc);
        }
        let lifetime = WindowLifetime::identify(NativeWindow(hwnd)).ok()?;
        let format = unsafe { GetPixelFormat(dc) };
        let entry = DRAWABLES.get(&(hwnd, lifetime, format))?;
        let mut drawable = entry.lock();
        drawable.contexts.insert(context as usize);
        Some(drawable.buffer.dc as Hdc)
    }

    pub fn window_extent(window: isize) -> Option<[u32; 2]> {
        if !Self::enabled() {
            return None;
        }
        DRAWABLES.iter().find_map(|entry| {
            if entry.key().0 != window {
                return None;
            }
            let extent = entry.lock().binding.extent();
            Some([extent.width, extent.height])
        })
    }

    pub fn current_dc(native: Hdc, read: bool) -> Hdc {
        if !Self::enabled() || native.is_null() {
            return native;
        }
        let binding = BINDING.get();
        if binding.2 == 0 {
            return native;
        }
        if read {
            binding.1 as Hdc
        } else {
            binding.0 as Hdc
        }
    }

    /// # Safety
    /// Called before the native swap on the application's presentation thread.
    pub unsafe fn present(dc: Hdc) -> Hdc {
        if !Self::enabled() {
            return dc;
        }
        Self::native(|| {
            let context = unsafe { current_context() } as usize;
            if context == 0 {
                return dc;
            }
            let actual = unsafe { wglGetCurrentDC() } as usize;
            if BINDING.get().0 != dc as usize {
                return dc;
            }
            for entry in DRAWABLES.iter() {
                let drawable = entry.lock();
                if drawable.buffer.dc != actual {
                    continue;
                }
                if drawable.buffer.lost() {
                    return std::ptr::null_mut();
                }
                let _read = match drawable
                    .read
                    .map(|read| unsafe { read.for_presentation(actual as Hdc, context as Hglrc) })
                    .transpose()
                {
                    Ok(binding) => binding,
                    Err(()) => return std::ptr::null_mut(),
                };
                let _presentation = super::PRESENTATION_LOCK.lock();
                let store = super::CAPTURES.get_or_init(Default::default);
                unsafe {
                    store.present_fixed(actual, context, &drawable.binding);
                }
                return actual as Hdc;
            }
            dc
        })
    }

    pub fn context_destroyed(context: usize) {
        Self::native(|| {
            DRAWABLES.retain(|_, entry| {
                let drawable = entry.get_mut();
                drawable.contexts.remove(&context);
                !drawable.contexts.is_empty()
            });
        });
        if BINDING.get().2 == context {
            BINDING.set((0, 0, 0));
        }
    }
}
