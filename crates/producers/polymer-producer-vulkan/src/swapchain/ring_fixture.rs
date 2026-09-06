use super::{PresentationSemaphores, SwapchainCopyEngine};
use crate::{ExportImageRing, ExportImageRingDesc, ExportTimelineSemaphore, ExternalHandleMode};
use ash::vk;
use polymer_presentation::{
    Extent2D, Rect,
    producer::{CapturedFrame, ExportDescriptor, FramePlacement},
};
use polymer_protocol::{AdapterLuid, ExternalImageKind, ExternalSyncKind, StreamId, SurfaceId};

pub(super) struct RingFixture;
impl RingFixture {
    pub(super) unsafe fn verify(
        instance: &ash::Instance,
        physical: vk::PhysicalDevice,
        device: &ash::Device,
        queue: vk::Queue,
        family: u32,
        source: vk::Image,
        format: vk::Format,
    ) {
        let _scope = polymer_presentation::observation::CaptureScope::enter();
        let extent = vk::Extent2D {
            width: 32,
            height: 48,
        };
        let memory = unsafe { instance.get_physical_device_memory_properties(physical) };
        let external_memory = ash::khr::external_memory_win32::Device::new(instance, device);
        let external_sync = ash::khr::external_semaphore_win32::Device::new(instance, device);
        let luid = AdapterLuid {
            low_part: 0,
            high_part: 0,
        };
        let ring = ExportImageRing::create(
            device.clone(),
            &external_memory,
            &memory,
            ExportImageRingDesc {
                extent,
                format,
                slots: 3,
                handle_mode: ExternalHandleMode::VulkanOpaque,
            },
        )
        .unwrap();
        let mut timelines = (0..2).map(|_| {
            ExportTimelineSemaphore::create_with_function(
                device.clone(),
                external_sync.fp().get_semaphore_win32_handle_khr,
                external_sync.fp().import_semaphore_win32_handle_khr,
                luid,
                ExternalHandleMode::VulkanOpaque,
            )
            .unwrap()
        });
        let pool = PresentationSemaphores::default();
        let mut engine = SwapchainCopyEngine::create(
            device.clone(),
            &pool,
            42,
            ring,
            timelines.next().unwrap(),
            Some(timelines.next().unwrap()),
            vec![source],
            extent,
            family,
            None,
        )
        .unwrap();
        let submit = |engine: &mut SwapchainCopyEngine| {
            engine.submit_image_copy_inner(
                queue,
                0,
                source,
                extent,
                vk::ImageLayout::TRANSFER_SRC_OPTIMAL,
                &[],
                false,
                vk::Offset2D { x: 4, y: 8 },
                (0, 0),
            )
        };
        for index in 0..3 {
            let frame = submit(&mut engine).unwrap();
            assert_eq!((frame.slot, frame.sync_value), (index, index as u64 + 1));
        }
        engine.wait_idle();
        assert_eq!(submit(&mut engine).err(), Some(vk::Result::NOT_READY));
        assert_eq!(
            engine.next_sync_value, 4,
            "busy capture consumed a synchronization value"
        );
        let (ring, producer, consumer) = engine.transport_parts();
        let handles = ring
            .slots()
            .iter()
            .map(|slot| slot.raw_handle() as i64)
            .collect::<Vec<_>>();
        let capture_extent = Extent2D {
            width: extent.width,
            height: extent.height,
        };
        let bounds = Rect {
            x: 0,
            y: 0,
            extent: capture_extent,
        };
        let polymer_ipc::Message::FrameReady(frame) = (CapturedFrame {
            stream_id: StreamId(42),
            slot: 0,
            extent: capture_extent,
            sync_value: 1,
            timestamp_qpc: 1,
        })
        .present(FramePlacement {
            extent: capture_extent,
            bounds,
            transform: FramePlacement::mapped(bounds, false, false),
        }) else {
            unreachable!()
        };
        polymer_test_viewer::OpaqueViewer::verify_export(
            ExportDescriptor {
                surface_id: SurfaceId(42),
                adapter_luid: luid,
                source_window: 0,
                width: extent.width,
                height: extent.height,
                format: ring.desc().format.as_raw() as u32,
                image_kind: ExternalImageKind::VulkanOpaqueWin32,
                sync_kind: ExternalSyncKind::VulkanTimelineSemaphore,
                image_handles: &handles,
                timeline_semaphore_handle: producer.raw_handle() as i64,
                consumer_semaphore_handle: consumer.unwrap().raw_handle() as i64,
                bottom_up: false,
            },
            &frame,
            true,
        );
        let resumed = submit(&mut engine).unwrap();
        assert_eq!((resumed.slot, resumed.sync_value), (0, 4));
        engine.wait_idle();
        assert_eq!(
            submit(&mut engine).err(),
            Some(vk::Result::NOT_READY),
            "releasing slot zero also released its siblings"
        );
    }
}
