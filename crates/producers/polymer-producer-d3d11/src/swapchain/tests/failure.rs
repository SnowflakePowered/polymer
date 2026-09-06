use super::*;
use std::time::{Duration, Instant};
use windows::Win32::{
    Foundation::HMODULE,
    Graphics::{
        Direct3D::{D3D_DRIVER_TYPE_WARP, D3D_FEATURE_LEVEL_11_0},
        Direct3D11::{D3D11_CREATE_DEVICE_BGRA_SUPPORT, D3D11_SDK_VERSION, D3D11CreateDevice},
        Dxgi::{
            Common::{DXGI_ALPHA_MODE_IGNORE, DXGI_FORMAT_B8G8R8A8_UNORM, DXGI_SAMPLE_DESC},
            CreateDXGIFactory2, DXGI_CREATE_FACTORY_FLAGS, DXGI_SWAP_CHAIN_DESC1,
            DXGI_SWAP_EFFECT_FLIP_SEQUENTIAL, DXGI_USAGE_RENDER_TARGET_OUTPUT, IDXGIFactory2,
        },
    },
};

struct Device(ID3D11Device, ID3D11DeviceContext4);

struct ResolveCapture(D3d11SwapchainCapture);
impl polymer_presentation::producer::CaptureBackend for ResolveCapture {
    fn connected(&self) -> bool {
        true
    }
}

struct ResolveSetup<'a>(&'a IDXGISwapChain, &'a TrackedResource);
impl polymer_presentation::producer::CaptureInitializer for ResolveSetup<'_> {
    type Backend = ResolveCapture;
    type Error = windows::core::Error;

    unsafe fn initialize(&self) -> windows::core::Result<Option<ResolveCapture>> {
        unsafe {
            D3d11SwapchainCapture::new(self.0.as_raw(), 1, D3d11RingMode::Fence, Some(self.1))
        }
        .map(|capture| Some(ResolveCapture(capture)))
    }
}

impl Device {
    unsafe fn swapchain(&self) -> IDXGISwapChain {
        unsafe {
            let factory: IDXGIFactory2 = CreateDXGIFactory2(DXGI_CREATE_FACTORY_FLAGS(0)).unwrap();
            let swapchain: IDXGISwapChain = factory
                .CreateSwapChainForComposition(
                    &self.0,
                    &DXGI_SWAP_CHAIN_DESC1 {
                        Width: 32,
                        Height: 32,
                        Format: DXGI_FORMAT_B8G8R8A8_UNORM,
                        SampleDesc: DXGI_SAMPLE_DESC {
                            Count: 1,
                            Quality: 0,
                        },
                        BufferUsage: DXGI_USAGE_RENDER_TARGET_OUTPUT,
                        BufferCount: 2,
                        SwapEffect: DXGI_SWAP_EFFECT_FLIP_SEQUENTIAL,
                        AlphaMode: DXGI_ALPHA_MODE_IGNORE,
                        ..Default::default()
                    },
                    None,
                )
                .unwrap()
                .cast()
                .unwrap();
            swapchain
        }
    }

    unsafe fn new() -> Self {
        let mut device = None;
        let mut context = None;
        unsafe {
            D3D11CreateDevice(
                None,
                D3D_DRIVER_TYPE_WARP,
                HMODULE::default(),
                D3D11_CREATE_DEVICE_BGRA_SUPPORT,
                Some(&[D3D_FEATURE_LEVEL_11_0]),
                D3D11_SDK_VERSION,
                Some(&mut device),
                None,
                Some(&mut context),
            )
        }
        .unwrap();
        Self(device.unwrap(), context.unwrap().cast().unwrap())
    }
}

#[test]
fn multisample_resolve_preserves_a_crop_without_allocation_padding() {
    use windows::Win32::Graphics::Direct3D11::*;
    unsafe {
        let device = Device::new();
        let swapchain = device.swapchain();
        let mut session = polymer_presentation::producer::CaptureSession::default();
        for (width, height, samples) in [(64, 64, 1), (64, 64, 2), (96, 80, 2), (80, 80, 1)] {
            let mut texture = None;
            device
                .0
                .CreateTexture2D(
                    &D3D11_TEXTURE2D_DESC {
                        Width: width,
                        Height: height,
                        MipLevels: 1,
                        ArraySize: 2,
                        Format: DXGI_FORMAT_B8G8R8A8_UNORM,
                        SampleDesc: DXGI_SAMPLE_DESC {
                            Count: samples,
                            Quality: 0,
                        },
                        Usage: D3D11_USAGE_DEFAULT,
                        BindFlags: D3D11_BIND_RENDER_TARGET.0 as u32,
                        ..Default::default()
                    },
                    None,
                    Some(&mut texture),
                )
                .unwrap();
            let texture = texture.unwrap();
            let mut view = None;
            device
                .0
                .CreateRenderTargetView(
                    &texture,
                    Some(&D3D11_RENDER_TARGET_VIEW_DESC {
                        Format: DXGI_FORMAT_B8G8R8A8_UNORM,
                        ViewDimension: if samples > 1 {
                            D3D11_RTV_DIMENSION_TEXTURE2DMSARRAY
                        } else {
                            D3D11_RTV_DIMENSION_TEXTURE2DARRAY
                        },
                        Anonymous: if samples > 1 {
                            D3D11_RENDER_TARGET_VIEW_DESC_0 {
                                Texture2DMSArray: D3D11_TEX2DMS_ARRAY_RTV {
                                    FirstArraySlice: 1,
                                    ArraySize: 1,
                                },
                            }
                        } else {
                            D3D11_RENDER_TARGET_VIEW_DESC_0 {
                                Texture2DArray: D3D11_TEX2D_ARRAY_RTV {
                                    MipSlice: 0,
                                    FirstArraySlice: 1,
                                    ArraySize: 1,
                                },
                            }
                        },
                    }),
                    Some(&mut view),
                )
                .unwrap();
            let view = view.unwrap();
            device.1.ClearRenderTargetView(&view, &[0.0, 0.0, 1.0, 1.0]);
            device.1.ClearView(
                &view,
                &[1.0, 0.0, 0.0, 1.0],
                Some(&[windows::Win32::Foundation::RECT {
                    left: 8,
                    top: 12,
                    right: 24,
                    bottom: 32,
                }]),
            );
            let mut source = TrackedResource::from_raw(texture.as_raw(), 1).unwrap();
            source.x = 8;
            source.y = 12;
            source.width = 16;
            source.height = 20;
            assert!(session.configure(crate::capture::SourceRequirements::from_source(&source)));
            session
                .initialize(Instant::now(), &ResolveSetup(&swapchain, &source))
                .unwrap();
            assert!(!session.configure(crate::capture::SourceRequirements::from_source(&source)));
            let capture = &mut session.as_mut().unwrap().0;
            let submission = capture.copy_frame(None, Some(&source)).unwrap();
            assert_eq!((submission.width, submission.height), (16, 20));
            let output = &capture.ring.slots()[submission.slot as usize].texture;
            let mut desc = D3D11_TEXTURE2D_DESC::default();
            output.GetDesc(&mut desc);
            assert_eq!(
                (desc.Width, desc.Height, desc.SampleDesc.Count),
                (16, 20, 1)
            );
            desc.Usage = D3D11_USAGE_STAGING;
            desc.BindFlags = 0;
            desc.MiscFlags = 0;
            desc.CPUAccessFlags = D3D11_CPU_ACCESS_READ.0 as u32;
            let mut staging = None;
            device
                .0
                .CreateTexture2D(&desc, None, Some(&mut staging))
                .unwrap();
            let staging = staging.unwrap();
            device.1.CopyResource(&staging, output);
            let mut mapped = D3D11_MAPPED_SUBRESOURCE::default();
            device
                .1
                .Map(&staging, 0, D3D11_MAP_READ, 0, Some(&mut mapped))
                .unwrap();
            let correct = (0..20).all(|y| {
                let row = std::slice::from_raw_parts(
                    mapped.pData.cast::<u8>().add(y * mapped.RowPitch as usize),
                    16 * 4,
                );
                row.chunks_exact(4).all(|pixel| pixel == [0, 0, 255, 255])
            });
            device.1.Unmap(&staging, 0);
            assert!(
                correct,
                "resolved output contains padding or the wrong crop"
            );
        }
    }
}

#[test]
fn failed_signal_retains_submitted_slot_and_recovery_does_not_require_unpublished_consumer_release()
{
    unsafe {
        let device = Device::new();
        let foreign = Device::new();
        let swapchain = device.swapchain();
        let identity = swapchain.cast::<windows::core::IUnknown>().unwrap();
        let references = (identity.vtable().AddRef)(identity.as_raw());
        (identity.vtable().Release)(identity.as_raw());
        let mut capture =
            D3d11SwapchainCapture::new(swapchain.as_raw(), 1, D3d11RingMode::Fence, None).unwrap();
        // A capture owned by the swapchain hook must not retain the swapchain
        // itself: that cycle prevents the hook's final-release cleanup.
        let capture_references = (identity.vtable().AddRef)(identity.as_raw());
        (identity.vtable().Release)(identity.as_raw());
        assert_eq!(capture_references, references);
        // Copy still runs on its original device. Signaling a fence through a
        // foreign device fails natively after the copy has been recorded.
        capture.context4 = Some(foreign.1.clone());
        assert!(capture.copy_frame(None, None).is_err());
        assert_eq!(capture.pending_signal, Some(1));
        assert_eq!(capture.submitted_values, [1]);
        assert_eq!(capture.release_values, [0]);
        assert!(capture.submitted_sources[0].is_some());
        assert!(capture.copy_frame(None, None).is_err());
        assert_eq!(capture.sync_value, 1);
        capture.context4 = Some(device.1.clone());
        let deadline = Instant::now() + Duration::from_secs(2);
        let submission = loop {
            match capture.copy_frame(None, None) {
                Ok(value) => break value,
                Err(error) => assert_eq!(error.code(), HRESULT(0x80070102u32 as i32)),
            }
            capture.context.Flush();
            assert!(
                Instant::now() < deadline,
                "pending D3D11 copy failed to recover"
            );
            std::thread::yield_now();
        };
        assert_eq!(submission.sync_value, 2);
        assert_eq!(capture.release_values, [2]);
        capture.context.Flush();
        while capture.ring.fence().unwrap().GetCompletedValue() < 2 {
            assert!(Instant::now() < deadline, "producer fence did not complete");
            std::thread::yield_now();
        }
        assert_eq!(
            capture.copy_frame(None, None).unwrap_err().code(),
            HRESULT(0x80070102u32 as i32)
        );
        assert!(capture.submitted_sources[0].is_none());
        drop(capture);

        // Retirement after a failed signal must release the capture's held
        // backbuffer references even though no frame was published. D3D11
        // defers native resource destruction for its queued copy commands.
        let mut abandoned =
            D3d11SwapchainCapture::new(swapchain.as_raw(), 1, D3d11RingMode::Fence, None).unwrap();
        abandoned.context4 = Some(foreign.1);
        assert!(abandoned.copy_frame(None, None).is_err());
        assert_eq!(abandoned.pending_signal, Some(1));
        drop(abandoned);
        device.1.Flush();
        swapchain
            .ResizeBuffers(
                0,
                48,
                32,
                windows::Win32::Graphics::Dxgi::Common::DXGI_FORMAT_UNKNOWN,
                windows::Win32::Graphics::Dxgi::DXGI_SWAP_CHAIN_FLAG(0),
            )
            .unwrap();
    }
}
