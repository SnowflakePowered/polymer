#![cfg(target_os = "macos")]

use objc2_metal::MTLCreateSystemDefaultDevice;
use polymer_renderer::{CompositingPlan, Extent};
use polymer_renderer_metal::MetalCompositor;
use std::time::{Duration, Instant};

#[test]
#[ignore = "requires a Metal device"]
fn unattached_layer_keeps_submitting_capture_work() {
    let device = MTLCreateSystemDefaultDevice().unwrap();
    // An unattached layer has no window to present into. Its drawable pool
    // must not gate GPU capture work after the first few submissions.
    let mut renderer = MetalCompositor::for_layer(device, Extent::new(64, 48)).unwrap();
    let plan = CompositingPlan::new([0.; 4], vec![]);
    let deadline = Instant::now() + Duration::from_secs(10);
    let mut captures = 0;
    while captures < 60 && Instant::now() < deadline {
        renderer
            .present_with_capture(
                &plan,
                |_| Ok(()),
                |_, _| {
                    captures += 1;
                    Ok(())
                },
            )
            .unwrap();
        std::thread::sleep(Duration::from_millis(2));
    }
    assert_eq!(captures, 60, "capture stalled behind onscreen presentation");
    while renderer.gpu_pending() && Instant::now() < deadline {
        std::thread::sleep(Duration::from_millis(2));
    }
    assert!(
        !renderer.gpu_pending(),
        "submitted GPU work did not complete"
    );
    renderer.check_submission().unwrap();
}
