use super::*;

#[test]
fn factory_scaling_and_tracked_zero_reach_decoder() {
    use polymer_bluetooth::devices::sony::dualshock3::motion::{
        MotionCalibration, MotionPath, MotionState,
    };
    let mut calibration = MotionCalibration {
        path: MotionPath::Hardware,
        acceleration_zero: [512, 514, 510],
        acceleration_span: [113, 112, 112],
        gyro_zero: 520.0,
        cal_byte: 0x7b,
        settling: false,
    };
    let mut decoder = DualShock3Decoder::default();
    decoder.set_ds3_motion(Some(MotionState::new(calibration)));
    let mut report = [0; 49];
    report[0] = 1;
    for (index, value) in [512u16, 514, 398, 643].into_iter().enumerate() {
        report[41 + index * 2..43 + index * 2].copy_from_slice(&value.to_be_bytes());
    }
    let state = decoder.decode(&report, Instant::now()).unwrap().state;
    assert_eq!(state.accelerometer, [0.0, 9.80665, 0.0]);
    assert_eq!(state.gyroscope, [0.0, std::f32::consts::FRAC_PI_2, 0.0]);
    calibration.settling = true;
    decoder.set_ds3_motion(Some(MotionState::new(calibration)));
    assert_eq!(
        decoder
            .decode(&report, Instant::now())
            .unwrap()
            .state
            .gyroscope,
        [0.0; 3]
    );
}

#[test]
fn digital_bits_map_by_position_without_treating_triggers_as_buttons() {
    use GamepadButton::*;
    let mut decoder = DualShock3Decoder::default();
    let mut report = [0; 49];
    report[0] = 1;
    for (bit, expected) in [
        (0, Select),
        (1, LeftStick),
        (2, RightStick),
        (3, Start),
        (4, DpadUp),
        (5, DpadRight),
        (6, DpadDown),
        (7, DpadLeft),
        (10, L),
        (11, R),
        (12, Y),
        (13, B),
        (14, A),
        (15, X),
        (16, Guide),
    ] {
        report[2..6].copy_from_slice(&(1u32 << bit).to_le_bytes());
        assert_eq!(
            decoder
                .decode(&report, Instant::now())
                .unwrap()
                .state
                .buttons,
            expected.mask()
        );
    }
    report[2..6].copy_from_slice(&[0, 3, 0, 0]);
    report[47..49].copy_from_slice(&389u16.to_be_bytes());
    let state = decoder.decode(&report, Instant::now()).unwrap().state;
    assert_eq!(state.buttons, 0);
    assert_eq!(state.gyroscope, [0.0, -std::f32::consts::FRAC_PI_2, 0.0]);
}

#[test]
fn native_report_preserves_pressure_and_motion_through_backend() {
    let mut report = [0u8; 49];
    report[0] = 1;
    report[2..6].copy_from_slice(&[0xff, 0xff, 1, 0]);
    report[6..10].copy_from_slice(&[0, 128, 255, 128]);
    report[14..26].copy_from_slice(&[1, 17, 32, 64, 128, 255, 63, 127, 191, 222, 0, 254]);
    report[30] = 4;
    report[41..49].copy_from_slice(&[0x02, 0x70, 0x01, 0x8e, 0x01, 0x1d, 0x02, 0x7b]);
    let mut decoder = super::super::for_profile(&polymer_bluetooth::devices::ProfileId::new(
        "sony.dualshock3",
    ))
    .unwrap();
    let decoded = decoder.decode(&report, Instant::now()).unwrap();
    let (state, _) = crate::BluetoothBackend::state(
        decoded.state,
        decoded.pointer_source,
        decoded.motion,
        decoded.battery_level,
        decoded.charging,
        [0; 2],
    );
    assert_eq!(state.gamepad.left_stick, [i16::MIN, 0]);
    assert_eq!(state.gamepad.right_stick, [i16::MAX, 0]);
    assert_eq!(
        (state.gamepad.left_trigger, state.gamepad.right_trigger),
        (32896, 65535)
    );
    use GamepadButton::*;
    for (button, pressure) in [
        (DpadUp, 1),
        (DpadRight, 17),
        (DpadDown, 32),
        (DpadLeft, 64),
        (L, 63),
        (R, 127),
        (Y, 191),
        (B, 222),
        (A, 0),
        (X, 254),
    ] {
        assert_eq!(state.gamepad.button_pressure(button), pressure);
        assert_ne!(state.gamepad.buttons & button.mask(), 0);
    }
    for button in [Select, Start, Guide, LeftStick, RightStick] {
        assert_eq!(state.gamepad.button_pressure(button), 255);
    }
    assert_eq!(state.gamepad.buttons, (1 << 15) - 1);
    for (actual, expected) in state
        .sensor
        .accelerometer
        .into_iter()
        .zip([9.80665, 19.6133, 9.80665])
    {
        assert!((actual - expected).abs() < 0.00001);
    }
    assert_eq!(
        state.sensor.gyroscope,
        [0.0, std::f32::consts::FRAC_PI_2, 0.0]
    );
    assert!(!state.sensor.has_orientation);
    assert_eq!(state.battery.level, Some(75));
    // A subsequent release must clear every pressure and digital button.
    report[2..6].fill(0);
    // Reproduce the controller retaining nonzero pressure after contact release.
    report[14..26].fill(3);
    let released = decoder.decode(&report, Instant::now()).unwrap().state;
    assert_eq!(released.buttons, 0);
    assert_eq!(released.pressure.values, [0; 21]);
    assert_eq!((released.left_trigger, released.right_trigger), (0, 0));
}

#[test]
fn rejects_incomplete_invalid_and_transport_framed_reports() {
    let mut decoder = DualShock3Decoder::default();
    let mut report = [0; 50];
    report[0] = 1;
    for len in (0..49).chain([50]) {
        assert!(decoder.decode(&report[..len], Instant::now()).is_none());
    }
    report[1] = 0xff;
    assert!(decoder.decode(&report[..49], Instant::now()).is_none());
    report[1] = 0;
    report[0] = 0xa1;
    assert!(decoder.decode(&report[..49], Instant::now()).is_none());
}

#[test]
fn distinguishes_charging_full_and_unknown_battery() {
    let mut decoder = DualShock3Decoder::default();
    let mut report = [0; 49];
    report[0] = 1;
    for (status, level, charging) in [
        (0, Some(0), false),
        (1, Some(1), false),
        (2, Some(25), false),
        (3, Some(50), false),
        (4, Some(75), false),
        (5, Some(100), false),
        (0xee, None, true),
        (0xef, Some(100), false),
        (0xff, None, false),
    ] {
        report[30] = status;
        let decoded = decoder.decode(&report, Instant::now()).unwrap();
        assert_eq!((decoded.battery_level, decoded.charging), (level, charging));
    }
}
