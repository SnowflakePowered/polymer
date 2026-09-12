use super::*;
use polymer_bluetooth::devices::sony::dualshock4::motion::FactoryCalibration;
use polymer_input::GamepadButton::*;

#[test]
fn distinguishes_full_battery_charge_errors_and_unplugged() {
    let mut decoder = DualShock4Decoder::default();
    for (data, expected) in [
        (include_bytes!("fixtures/full.bin"), Some(100)),
        (include_bytes!("fixtures/charge_error.bin"), None),
        (include_bytes!("fixtures/unplugged.bin"), Some(75)),
    ] {
        let input = decoder.decode(data, Instant::now()).unwrap();
        assert_eq!(input.battery_level, expected);
        assert!(!input.charging);
    }
}

// Synthetic wire vectors with CRCs generated independently using Python zlib.
#[test]
fn controls_motion_touch_history_and_timestamp_wrap() {
    let mut decoder = DualShock4Decoder::default();
    decoder.set_sensor_calibration(FactoryCalibration::parse(include_bytes!(
        "fixtures/calibration.bin"
    )));
    let first = decoder
        .decode(include_bytes!("fixtures/input.bin"), Instant::now())
        .unwrap();
    assert_eq!(
        first.state.buttons,
        A.mask()
            | L.mask()
            | Select.mask()
            | Start.mask()
            | Guide.mask()
            | Touchpad.mask()
            | DpadUp.mask()
            | DpadRight.mask()
    );
    assert_eq!(first.state.left_stick, [i16::MIN, 0]);
    assert_eq!(first.state.right_stick, [i16::MAX, 0]);
    assert_eq!(
        (first.state.left_trigger, first.state.right_trigger),
        (65535, 32896)
    );
    assert_eq!(first.battery_level, Some(75));
    assert!(first.charging);
    assert_eq!(first.state.accelerometer, [0.0, 9.80665, 0.0]);
    assert!((first.state.gyroscope[0] - std::f32::consts::FRAC_PI_2).abs() < 0.00001);
    assert_eq!(first.state.touch_contacts.unwrap()[0].position, [1000, 500]);
    let swapped = decoder
        .decode(include_bytes!("fixtures/swapped.bin"), Instant::now())
        .unwrap();
    assert_eq!(swapped.state.sensor_timestamp_ns, Some(48_000));
    assert_eq!(swapped.state.pointer, first.state.pointer);
    let idle = decoder
        .decode(include_bytes!("fixtures/no_touch.bin"), Instant::now())
        .unwrap();
    assert_eq!(idle.state.touch_contacts, swapped.state.touch_contacts);
    let released = decoder
        .decode(include_bytes!("fixtures/released.bin"), Instant::now())
        .unwrap();
    assert_eq!(released.state.pointer, first.state.pointer);
    assert!(
        released
            .state
            .touch_contacts
            .unwrap()
            .iter()
            .all(|c| !c.active)
    );
}

#[test]
fn malformed_input_cannot_change_sensor_clock() {
    let mut decoder = DualShock4Decoder::default();
    let first = include_bytes!("fixtures/input.bin");
    decoder.decode(first, Instant::now()).unwrap();
    assert!(decoder.decode(&first[..77], Instant::now()).is_none());
    let mut corrupt = *include_bytes!("fixtures/swapped.bin");
    corrupt[12] ^= 1;
    assert!(decoder.decode(&corrupt, Instant::now()).is_none());
    assert!(
        decoder
            .decode(
                include_bytes!("fixtures/bad_touch_count.bin"),
                Instant::now()
            )
            .is_none()
    );
    let valid = decoder
        .decode(include_bytes!("fixtures/swapped.bin"), Instant::now())
        .unwrap();
    assert_eq!(valid.state.sensor_timestamp_ns, Some(48_000));
    assert!((valid.state.accelerometer[1] - 7992.0 * 9.80665 / 8192.0).abs() < 0.00001);
}
