use super::*;

impl NativeCaptureFixture {
    pub(super) unsafe fn verify_external(
        device: &windows::Win32::Graphics::Direct3D12::ID3D12Device,
        ready: &StreamReady,
        frame: &polymer_protocol::FrameReady,
    ) {
        use windows::Win32::Graphics::Direct3D12::*;
        unsafe {
            let mut image: Option<ID3D12Resource> = None;
            let mut producer: Option<ID3D12Fence> = None;
            let mut consumer: Option<ID3D12Fence> = None;
            device
                .OpenSharedHandle(
                    HANDLE(ready.image_handles[frame.slot as usize] as *mut _),
                    &mut image,
                )
                .unwrap();
            device
                .OpenSharedHandle(
                    HANDLE(ready.timeline_semaphore_handle as *mut _),
                    &mut producer,
                )
                .unwrap();
            device
                .OpenSharedHandle(
                    HANDLE(ready.consumer_semaphore_handle as *mut _),
                    &mut consumer,
                )
                .unwrap();
            let queue: ID3D12CommandQueue = device
                .CreateCommandQueue(&D3D12_COMMAND_QUEUE_DESC {
                    Type: D3D12_COMMAND_LIST_TYPE_DIRECT,
                    ..Default::default()
                })
                .unwrap();
            queue.Wait(&producer.unwrap(), frame.sync_value).unwrap();
            let pixels = polymer_presentation::producer::testing::D3d12Readback::pixels(
                device,
                &queue,
                &image.unwrap(),
                frame.render_width,
                frame.render_height,
            );
            for (y, row) in pixels.chunks_exact(frame.render_width as usize).enumerate() {
                assert!(
                    row.iter().all(|pixel| *pixel
                        == if y < 6 {
                            [255, 0, 0, 255]
                        } else {
                            [0, 255, 0, 255]
                        }),
                    "external image includes padding or the wrong crop at row {y}"
                );
            }
            // Readback has completed on the CPU, so all consumer accesses have
            // finished before releasing this output to the producer.
            consumer.unwrap().Signal(frame.sync_value).unwrap();
        }
    }
}
