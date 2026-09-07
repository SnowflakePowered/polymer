use super::*;
use crate::{
    layout::Rect,
    renderer::{CompositingPlan, SurfacePlacement, VulkanCompositor},
};
use windows::{
    Win32::{
        Foundation::HMODULE,
        Graphics::{Direct3D::*, Direct3D11::*, Dxgi::*},
    },
    core::Interface,
};

struct KeyedProducer {
    device: ID3D11Device5,
    context: ID3D11DeviceContext4,
    images: Vec<ID3D11Texture2D>,
    ready: ID3D11Fence,
    adapter: AdapterLuid,
}

impl KeyedProducer {
    fn new() -> Self {
        unsafe {
            let mut device = None;
            let mut context = None;
            D3D11CreateDevice(
                None,
                D3D_DRIVER_TYPE_HARDWARE,
                HMODULE::default(),
                D3D11_CREATE_DEVICE_BGRA_SUPPORT,
                None,
                D3D11_SDK_VERSION,
                Some(&mut device),
                None,
                Some(&mut context),
            )
            .unwrap();
            let device: ID3D11Device5 = device.unwrap().cast().unwrap();
            let context: ID3D11DeviceContext4 = context.unwrap().cast().unwrap();
            let adapter = device
                .cast::<IDXGIDevice>()
                .unwrap()
                .GetAdapter()
                .unwrap()
                .GetDesc()
                .unwrap()
                .AdapterLuid;
            let mut images = Vec::new();
            for _ in 0..3 {
                let mut image = None;
                device
                    .CreateTexture2D(
                        &D3D11_TEXTURE2D_DESC {
                            Width: 16,
                            Height: 16,
                            MipLevels: 1,
                            ArraySize: 1,
                            Format: DXGI_FORMAT_R8G8B8A8_UNORM,
                            SampleDesc: DXGI_SAMPLE_DESC {
                                Count: 1,
                                Quality: 0,
                            },
                            Usage: D3D11_USAGE_DEFAULT,
                            BindFlags: (D3D11_BIND_RENDER_TARGET | D3D11_BIND_SHADER_RESOURCE).0
                                as u32,
                            MiscFlags: (D3D11_RESOURCE_MISC_SHARED_NTHANDLE
                                | D3D11_RESOURCE_MISC_SHARED_KEYEDMUTEX)
                                .0 as u32,
                            ..Default::default()
                        },
                        None,
                        Some(&mut image),
                    )
                    .unwrap();
                images.push(image.unwrap());
            }
            let mut ready = None;
            device
                .CreateFence(0, D3D11_FENCE_FLAG_SHARED, &mut ready)
                .unwrap();
            let ready = ready.unwrap();
            Self {
                device,
                context,
                images,
                ready,
                adapter: AdapterLuid {
                    low_part: adapter.LowPart,
                    high_part: adapter.HighPart,
                },
            }
        }
    }

    fn stream(&self) -> StreamReady {
        let mut handles = [0; MAX_IMAGE_SLOTS];
        unsafe {
            for (index, image) in self.images.iter().enumerate() {
                handles[index] = image
                    .cast::<IDXGIResource1>()
                    .unwrap()
                    .CreateSharedHandle(None, 0x10000000, windows::core::PCWSTR::null())
                    .unwrap()
                    .0 as i64;
            }
            StreamReady {
                producer_process_id: std::process::id(),
                surface_id: SurfaceId(1),
                adapter_luid: self.adapter,
                source_window: 0,
                width: 16,
                height: 16,
                format: DXGI_FORMAT_R8G8B8A8_UNORM.0 as u32,
                image_kind: ExternalImageKind::D3d11SharedResourceNt,
                sync_kind: ExternalSyncKind::D3d11Fence,
                slot_count: 3,
                image_handles: handles,
                timeline_semaphore_handle: self
                    .ready
                    .CreateSharedHandle(None, 0x10000000, windows::core::PCWSTR::null())
                    .unwrap()
                    .0 as i64,
                consumer_semaphore_handle: 0,
                bottom_up: false,
            }
        }
    }

    fn available(&self, slot: usize) -> bool {
        let mutex: IDXGIKeyedMutex = self.images[slot].cast().unwrap();
        unsafe {
            let result = (Interface::vtable(&mutex).AcquireSync)(Interface::as_raw(&mutex), 0, 0);
            if result.0 != 0 {
                return false;
            }
            mutex.ReleaseSync(0).unwrap();
        }
        true
    }

    fn paint(&self, slot: usize, value: u64) -> FrameReady {
        let image = &self.images[slot];
        let mutex: IDXGIKeyedMutex = image.cast().unwrap();
        unsafe {
            assert_eq!(
                (Interface::vtable(&mutex).AcquireSync)(Interface::as_raw(&mutex), 0, 0).0,
                0
            );
            let mut view = None;
            self.device
                .CreateRenderTargetView(image, None, Some(&mut view))
                .unwrap();
            self.context
                .ClearRenderTargetView(view.as_ref().unwrap(), &[1.0, 0.0, 0.0, 1.0]);
            self.context.Signal(&self.ready, value).unwrap();
            mutex.ReleaseSync(1).unwrap();
            self.context.Flush();
            let deadline = Instant::now() + Duration::from_secs(5);
            while self.ready.GetCompletedValue() < value {
                assert!(Instant::now() < deadline);
                std::thread::sleep(Duration::from_millis(1));
            }
        }
        Producer::frame(slot, value)
    }
}

#[test]
#[ignore = "requires Windows Vulkan/D3D11 keyed-mutex interoperability"]
fn keyed_lease_survives_redraw_and_recycles_skipped_frames() {
    let producer = KeyedProducer::new();
    let window = TestWindow::new();
    let mut compositor = VulkanCompositor::new(&window, Some(producer.adapter), true).unwrap();
    compositor.import_stream(1, &producer.stream(), 0).unwrap();
    let plan = CompositingPlan::new(
        [0.0, 0.0, 0.0, 1.0],
        vec![SurfacePlacement::new(1, 1, Rect::new(0, 0, 16, 16), 0.0)],
    );
    compositor.set_latest(1, producer.paint(0, 1));
    compositor.present(&plan).unwrap();
    compositor.present(&plan).unwrap();
    unsafe {
        compositor
            .device()
            .queue_wait_idle(compositor.queue())
            .unwrap();
    }
    assert!(!producer.available(0));
    compositor.set_latest(1, producer.paint(1, 2));
    let last = producer.paint(2, 3);
    compositor.set_latest(1, last);
    compositor.present(&plan).unwrap();
    unsafe {
        compositor
            .device()
            .queue_wait_idle(compositor.queue())
            .unwrap();
    }
    assert!(producer.available(0));
    assert!(producer.available(1));
    assert!(!producer.available(2));
    compositor.set_latest(1, last); // Duplicate notification must not reacquire.
    compositor.present(&plan).unwrap();
    compositor.disconnect_stream(1).unwrap();
    assert!(producer.available(2));
}
