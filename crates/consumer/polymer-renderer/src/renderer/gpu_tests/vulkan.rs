use super::*;
use crate::{
    layout::Rect,
    renderer::{CompositingPlan, SurfacePlacement, VulkanCompositor},
};

#[test]
#[ignore = "requires Windows Vulkan/D3D12 external-memory interoperability"]
fn vulkan_retains_until_replacement_and_returns_ownership_on_disconnect() {
    let adapter = D3d12ConsumerDevice::new_default().unwrap().adapter_luid();
    let producer = Producer::new(adapter, 3);
    let window = TestWindow::new();
    let mut compositor = VulkanCompositor::new(&window, Some(adapter), false).unwrap();
    compositor.import_stream(1, &producer.stream(), 0).unwrap();
    let plan = CompositingPlan::new(
        [0.0, 0.0, 0.0, 1.0],
        vec![SurfacePlacement::new(1, 1, Rect::new(0, 0, 16, 16), 0.0)],
    );
    compositor.set_latest(1, producer.paint(0, 1, [1.0, 0.0, 0.0, 1.0]));
    compositor.present(&plan).unwrap();
    compositor.set_latest(1, Producer::frame(1, 2)); // Producer never signals frame 2.
    compositor.present(&plan).unwrap();
    unsafe {
        compositor
            .device()
            .queue_wait_idle(compositor.queue())
            .unwrap();
    }
    assert_eq!(unsafe { producer.released.GetCompletedValue() }, 0);
    compositor.set_latest(1, producer.paint(2, 3, [0.0, 0.0, 1.0, 1.0]));
    compositor.present(&plan).unwrap();
    Producer::wait(&producer.released, 2);
    assert_eq!(unsafe { producer.released.GetCompletedValue() }, 2);
    let next = producer.paint(0, 4, [0.0, 1.0, 0.0, 1.0]);
    compositor.present(&plan).unwrap(); // Redraw without reacquiring external ownership.
    compositor.set_latest(1, next);
    compositor.present(&plan).unwrap();
    Producer::wait(&producer.released, 3);
    assert_eq!(unsafe { producer.released.GetCompletedValue() }, 3);
    compositor.disconnect_stream(1).unwrap();
    assert!(unsafe { producer.released.GetCompletedValue() } >= 4);
}

#[test]
#[ignore = "requires Windows Vulkan/D3D12 external-memory interoperability"]
fn vulkan_single_slot_keeps_producer_progressing() {
    let adapter = D3d12ConsumerDevice::new_default().unwrap().adapter_luid();
    let producer = Producer::new(adapter, 1);
    let window = TestWindow::new();
    let mut compositor = VulkanCompositor::new(&window, Some(adapter), false).unwrap();
    compositor.import_stream(1, &producer.stream(), 0).unwrap();
    let plan = CompositingPlan::new(
        [0.0, 0.0, 0.0, 1.0],
        vec![SurfacePlacement::new(1, 1, Rect::new(0, 0, 16, 16), 0.0)],
    );
    for value in 1..=4 {
        compositor.set_latest(1, producer.paint(0, value, [0.0, 1.0, 0.0, 1.0]));
        compositor.present(&plan).unwrap();
        Producer::wait(&producer.released, value);
        compositor.present(&plan).unwrap();
    }
    compositor.disconnect_stream(1).unwrap();
}
