use super::{CreateInfo, D3d11VirtualBackend, frontend::D3d11Frontend};
use polymer_hook_dxgi::{SwapchainCreateInfo, swapchain::VirtualDxgiSwapchain};
use polymer_platform_windows::{NativeWindow, WindowLifetime};
use polymer_presentation::{
    producer::{ProducerSession, ProducerWindows},
    swapchain::*,
};
use std::{
    ffi::c_void,
    sync::{
        Arc, LazyLock,
        atomic::{AtomicU64, Ordering},
    },
};
use windows::{
    Win32::{
        Foundation::*,
        Graphics::{Direct3D11::*, Dxgi::*},
    },
    core::{Error, IUnknown, Interface},
};

static ASSIGNMENTS: LazyLock<Result<Arc<AssignmentRegistry>, String>> = LazyLock::new(|| {
    AssignmentRegistry::new(polymer_hook::config::render_configuration().map_err(str::to_owned)?)
});
static NEXT_SOURCE: AtomicU64 = AtomicU64::new(1);

pub struct D3d11Creation;
impl D3d11Creation {
    /// # Safety
    /// Arguments follow the DXGI creation callback contract.
    pub unsafe extern "system" fn claim(
        info: *const SwapchainCreateInfo,
        output: *mut *mut c_void,
    ) -> i32 {
        if info.is_null() || output.is_null() {
            return E_POINTER.0;
        }
        let result = std::panic::catch_unwind(|| unsafe { Self::create(&*info) });
        match result {
            Ok(Ok(Some(swapchain))) => {
                unsafe {
                    output.write(swapchain.into_raw());
                }
                S_OK.0
            }
            Ok(Ok(None)) => S_FALSE.0,
            Ok(Err(error)) => {
                tracing::error!(%error, "virtual D3D11 creation rejected");
                error.code().0
            }
            Err(_) => E_FAIL.0,
        }
    }

    unsafe fn create(info: &SwapchainCreateInfo) -> windows::core::Result<Option<IDXGISwapChain3>> {
        let Some(unknown) = (unsafe { IUnknown::from_raw_borrowed(&info.device_or_queue) }) else {
            return Ok(None);
        };
        let Ok(device) = unknown.cast::<ID3D11Device>() else {
            return Ok(None);
        };
        let context = unsafe { device.GetImmediateContext()? };
        let window = NativeWindow(info.window as isize);
        let lifetime = WindowLifetime::identify(window)?;
        let serial = NEXT_SOURCE
            .fetch_update(Ordering::Relaxed, Ordering::Relaxed, |value| {
                value.checked_add(1)
            })
            .map_err(|_| Error::new(E_FAIL, "virtual D3D11 source identity exhausted"))?;
        let source = SourceIdentity {
            device: device.cast::<IUnknown>()?.as_raw() as u64,
            object: serial,
            lifetime: serial,
        };
        let registry = ASSIGNMENTS
            .as_ref()
            .map_err(|error| Error::new(E_INVALIDARG, error))?;
        let registration = registry
            .register(Candidate {
                source,
                window: WindowIdentity {
                    handle: info.window as u64,
                    lifetime,
                },
                title: polymer_platform_windows::window_title(window).unwrap_or_default(),
            })
            .map_err(|error| Error::new(E_FAIL, error))?;
        let Some(reservation) = registration.reserve() else {
            return Ok(None);
        };
        let binding = reservation.shared_binding();
        let description = Self::description(info, &binding)?;
        let factory = unsafe { IUnknown::from_raw_borrowed(&info.factory) }
            .ok_or_else(|| Error::from_hresult(E_POINTER))?
            .clone();
        let fixed = ProducerWindows::bind_fixed(&binding)?;
        let session = ProducerSession::connect(
            &polymer_hook::config::session()
                .map_err(|error| Error::new(E_FAIL, error.to_string()))?,
        )
        .map_err(|error| Error::new(E_FAIL, error.to_string()))?;
        let controller = SwapchainController::<D3d11VirtualBackend>::create(
            CreateInfo {
                device: device.clone(),
                format: description.Format,
                image_count: description.BufferCount,
            },
            binding,
            session,
        )
        .map_err(|failure| {
            Error::new(E_FAIL, format!("virtual stream setup: {:?}", failure.error))
        })?;
        let mut driver = D3d11Frontend::new(controller, context)?;
        Arc::get_mut(&mut driver).expect("new frontend").bind(
            registration,
            fixed,
            description.Flags & DXGI_SWAP_CHAIN_FLAG_FRAME_LATENCY_WAITABLE_OBJECT.0 as u32 != 0,
        )?;
        let object = VirtualDxgiSwapchain::create(
            factory,
            device.cast()?,
            HWND(info.window),
            description,
            driver.clone(),
        )?;
        reservation
            .commit()
            .map_err(|error| Error::new(E_FAIL, error))?;
        driver.register()?;
        polymer_platform_windows::resize_client(window, description.Width, description.Height)
            .map_err(|error| Error::new(E_FAIL, error.to_string()))?;
        Ok(Some(object))
    }

    fn description(
        info: &SwapchainCreateInfo,
        binding: &FixedSurfaceBinding,
    ) -> windows::core::Result<DXGI_SWAP_CHAIN_DESC1> {
        if !info.restrict_to_output.is_null()
            || unsafe { info.fullscreen.as_ref() }
                .is_some_and(|fullscreen| !fullscreen.Windowed.as_bool())
        {
            return Err(Error::new(
                DXGI_ERROR_UNSUPPORTED,
                "virtual D3D11 requires unrestricted windowed presentation",
            ));
        }
        let mut description = info.description;
        description.Width = binding.extent().width;
        description.Height = binding.extent().height;
        VirtualDxgiSwapchain::validate(HWND(info.window), &description)?;
        let supported = DXGI_USAGE_RENDER_TARGET_OUTPUT | DXGI_USAGE_SHADER_INPUT;
        if description.BufferUsage.0 & !supported.0 != 0
            || !description
                .BufferUsage
                .contains(DXGI_USAGE_RENDER_TARGET_OUTPUT)
        {
            return Err(Error::new(
                DXGI_ERROR_UNSUPPORTED,
                "unsupported virtual D3D11 buffer usage",
            ));
        }
        Ok(description)
    }
}
