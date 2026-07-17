//! Short-lived D3D11 swapchain used for end-to-end Snowglass validation.

use std::{error::Error, time::Duration};
use windows::{
    Win32::Graphics::{
        Direct3D::{D3D_DRIVER_TYPE_HARDWARE, D3D_FEATURE_LEVEL},
        Direct3D11::{
            D3D11_CREATE_DEVICE_FLAG, D3D11_SDK_VERSION, D3D11CreateDevice, ID3D11Device,
            ID3D11RenderTargetView, ID3D11Texture2D,
        },
        Dxgi::{
            Common::{
                DXGI_ALPHA_MODE_UNSPECIFIED, DXGI_FORMAT_B8G8R8A8_UNORM, DXGI_FORMAT_UNKNOWN,
                DXGI_SAMPLE_DESC,
            },
            DXGI_PRESENT, DXGI_PRESENT_PARAMETERS, DXGI_SCALING_STRETCH, DXGI_SWAP_CHAIN_DESC1,
            DXGI_SWAP_CHAIN_FLAG, DXGI_SWAP_EFFECT_DISCARD, DXGI_USAGE_RENDER_TARGET_OUTPUT,
            IDXGIAdapter, IDXGIDevice, IDXGIFactory2, IDXGIOutput, IDXGISwapChain,
        },
    },
    core::{Error as WindowsError, HRESULT, Interface},
};
use windows_sys::Win32::{
    System::LibraryLoader::GetModuleHandleW,
    UI::WindowsAndMessaging::{
        CW_USEDEFAULT, CreateWindowExW, DestroyWindow, SW_SHOW, ShowWindow, WS_OVERLAPPEDWINDOW,
    },
};

fn main() -> Result<(), Box<dyn Error>> {
    unsafe { run() }
}

unsafe fn run() -> Result<(), Box<dyn Error>> {
    let class = "STATIC\0".encode_utf16().collect::<Vec<_>>();
    let title = "Snowglass D3D11 Smoke Target\0"
        .encode_utf16()
        .collect::<Vec<_>>();
    let window = unsafe {
        CreateWindowExW(
            0,
            class.as_ptr(),
            title.as_ptr(),
            WS_OVERLAPPEDWINDOW,
            CW_USEDEFAULT,
            CW_USEDEFAULT,
            640,
            480,
            std::ptr::null_mut(),
            std::ptr::null_mut(),
            GetModuleHandleW(std::ptr::null()),
            std::ptr::null(),
        )
    };
    if window.is_null() {
        return Err(std::io::Error::last_os_error().into());
    }
    unsafe { ShowWindow(window, SW_SHOW) };

    let desc = DXGI_SWAP_CHAIN_DESC1 {
        Width: 640,
        Height: 480,
        Format: DXGI_FORMAT_B8G8R8A8_UNORM,
        Stereo: false.into(),
        SampleDesc: DXGI_SAMPLE_DESC {
            Count: 1,
            Quality: 0,
        },
        BufferUsage: DXGI_USAGE_RENDER_TARGET_OUTPUT,
        BufferCount: 2,
        Scaling: DXGI_SCALING_STRETCH,
        SwapEffect: DXGI_SWAP_EFFECT_DISCARD,
        AlphaMode: DXGI_ALPHA_MODE_UNSPECIFIED,
        Flags: 0,
    };
    let mut device = None;
    let mut context = None;
    let mut feature_level = D3D_FEATURE_LEVEL::default();
    unsafe {
        D3D11CreateDevice(
            None::<&IDXGIAdapter>,
            D3D_DRIVER_TYPE_HARDWARE,
            windows::Win32::Foundation::HMODULE::default(),
            D3D11_CREATE_DEVICE_FLAG(0),
            None,
            D3D11_SDK_VERSION,
            Some(&mut device),
            Some(&mut feature_level),
            Some(&mut context),
        )?
    };
    let device = required(device, "D3D11 did not return a device")?;
    let context = required(context, "D3D11 did not return a context")?;
    let dxgi_device: IDXGIDevice = device.cast()?;
    let adapter = unsafe { dxgi_device.GetAdapter()? };
    let factory = unsafe { adapter.GetParent::<IDXGIFactory2>()? };
    let swapchain1 = unsafe {
        factory.CreateSwapChainForHwnd(
            &device,
            windows::Win32::Foundation::HWND(window),
            &raw const desc,
            None,
            None::<&IDXGIOutput>,
        )?
    };
    let swapchain: IDXGISwapChain = swapchain1.cast()?;
    let mut target = unsafe { create_target(&device, &swapchain)? };
    let present_parameters = DXGI_PRESENT_PARAMETERS::default();

    for frame in 0..180u32 {
        if frame == 90 {
            drop(target);
            unsafe {
                swapchain1.ResizeBuffers(
                    0,
                    800,
                    450,
                    DXGI_FORMAT_UNKNOWN,
                    DXGI_SWAP_CHAIN_FLAG(0),
                )?
            };
            target = unsafe { create_target(&device, &swapchain)? };
        }
        let phase = frame as f32 / 179.0;
        unsafe {
            context.ClearRenderTargetView(&target, &[phase, 0.15, 1.0 - phase, 1.0]);
            swapchain1
                .Present1(0, DXGI_PRESENT(0), &raw const present_parameters)
                .ok()?;
        }
        std::thread::sleep(Duration::from_millis(8));
    }

    drop(target);
    drop(context);
    drop(device);
    drop(swapchain);
    drop(swapchain1);
    unsafe { DestroyWindow(window) };
    Ok(())
}

unsafe fn create_target(
    device: &ID3D11Device,
    swapchain: &IDXGISwapChain,
) -> windows::core::Result<ID3D11RenderTargetView> {
    let buffer: ID3D11Texture2D = unsafe { swapchain.GetBuffer(0)? };
    let mut target = None;
    unsafe { device.CreateRenderTargetView(&buffer, None, Some(&mut target))? };
    required(target, "D3D11 did not return a render-target view")
}

fn required<T>(value: Option<T>, message: &str) -> windows::core::Result<T> {
    value.ok_or_else(|| WindowsError::new(HRESULT(0x8000_4003u32 as i32), message))
}
