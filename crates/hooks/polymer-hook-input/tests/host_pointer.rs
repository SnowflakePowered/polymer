use polymer_hook_input::HostState;
use polymer_ipc::input::{InputMessage, WireHostPointer};

#[test]
fn surface_updates_preserve_raw_motion_until_the_next_motion_sample() {
    let state = HostState::default();
    let motion = WireHostPointer {
        surface_id: 0,
        target_window: 0,
        source: 1,
        phase: 1,
        pointer_id: 0,
        x: 0,
        y: 0,
        coordinate_width: 0,
        coordinate_height: 0,
        button: 0,
        position: [0.0; 2],
        delta: [0.5, -0.25],
        buttons: 0,
        wheel_horizontal: 0,
        wheel_vertical: 0,
        timestamp_ms: 0,
    };
    state.receive(InputMessage::HostPointer(motion));
    state.receive(InputMessage::HostPointer(WireHostPointer {
        surface_id: 1,
        coordinate_width: 1280,
        coordinate_height: 960,
        position: [1.0, 0.0],
        delta: [0.0; 2],
        buttons: 1,
        ..motion
    }));
    let sample = state.sample();
    assert!(sample.delta[0] > 0.0 && sample.delta[1] < 0.0);
    assert_eq!(sample.pointer, Some([i16::MAX, i16::MIN]));
    assert_eq!(sample.mouse_buttons, 1);
    state.receive(InputMessage::HostPointer(WireHostPointer {
        delta: [0.0; 2],
        ..motion
    }));
    assert_eq!(state.sample().delta, [0.0; 2]);
}
