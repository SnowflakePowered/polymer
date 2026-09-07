use super::*;
use crate::{
    layout::Rect,
    renderer::{CompositingPlan, D3d12Compositor, SurfacePlacement},
};

struct Display {
    compositor: D3d12Compositor,
    plan: CompositingPlan,
    _window: TestWindow,
}

impl Display {
    fn new() -> Self {
        let window = TestWindow::new();
        let compositor = D3d12Compositor::new_default(&window).unwrap();
        let plan = CompositingPlan::new(
            [0.0, 0.0, 0.0, 1.0],
            vec![SurfacePlacement::new(1, 1, Rect::new(0, 0, 16, 16), 0.0)],
        );
        Self {
            compositor,
            plan,
            _window: window,
        }
    }

    fn present(&mut self, expected: [u8; 4]) {
        self.compositor.present(&self.plan).unwrap();
        let (_, _, pixels) = self.compositor.snapshot_rgba().unwrap().unwrap();
        assert!(
            pixels.chunks_exact(4).all(|pixel| pixel == expected),
            "displayed frame changed while retained"
        );
    }
}

#[test]
#[ignore = "requires a Windows D3D12 GPU"]
fn fence_slot_survives_redraw_pending_frames_and_replacement() {
    let mut display = Display::new();
    let producer = Producer::new(display.compositor.adapter_luid(), 3);
    display
        .compositor
        .import_stream(1, &producer.stream(), 0)
        .unwrap();
    let red = producer.paint(0, 1, [1.0, 0.0, 0.0, 1.0]);
    display.compositor.set_latest(1, red);
    display.present([255, 0, 0, 255]);
    assert_eq!(unsafe { producer.released.GetCompletedValue() }, 0);
    display.compositor.set_latest(1, Producer::frame(1, 2)); // Not GPU-ready.
    display.present([255, 0, 0, 255]);
    assert_eq!(unsafe { producer.released.GetCompletedValue() }, 0);
    let blue = producer.paint(2, 3, [0.0, 0.0, 1.0, 1.0]);
    display.compositor.set_latest(1, blue); // Drops frame 2.
    display.present([0, 0, 255, 255]);
    assert_eq!(unsafe { producer.released.GetCompletedValue() }, 2);
    let green = producer.paint(0, 4, [0.0, 1.0, 0.0, 1.0]);
    display.present([0, 0, 255, 255]);
    display.compositor.set_latest(1, green);
    display.present([0, 255, 0, 255]);
    assert_eq!(unsafe { producer.released.GetCompletedValue() }, 3);
    display.compositor.disconnect_stream(1).unwrap();
    assert_eq!(
        unsafe { producer.released.GetCompletedValue() },
        polymer_protocol::RELEASE_ALL_SYNC_VALUE
    );
}

#[test]
#[ignore = "requires a Windows D3D12 GPU"]
fn single_slot_cache_allows_repaint_and_keeps_last_frame() {
    let mut display = Display::new();
    let producer = Producer::new(display.compositor.adapter_luid(), 1);
    display
        .compositor
        .import_stream(1, &producer.stream(), 0)
        .unwrap();
    display
        .compositor
        .set_latest(1, producer.paint(0, 1, [1.0, 0.0, 0.0, 1.0]));
    display.present([255, 0, 0, 255]);
    assert_eq!(unsafe { producer.released.GetCompletedValue() }, 1);
    let green = producer.paint(0, 2, [0.0, 1.0, 0.0, 1.0]);
    display.present([255, 0, 0, 255]);
    display.compositor.set_latest(1, green);
    display.present([0, 255, 0, 255]);
    assert_eq!(unsafe { producer.released.GetCompletedValue() }, 2);
}
