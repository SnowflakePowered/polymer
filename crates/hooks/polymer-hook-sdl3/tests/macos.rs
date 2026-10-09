#![cfg(target_os = "macos")]
use polymer_controllers::topology::{InputTopology, PortBinding, RemapMode};
use polymer_ipc::{
    BlobKind, LocalSocketListener, Message,
    input::{InputMessage, InputPublisher, WireDeviceConnected, WireDeviceState, WireHostPointer},
};
use std::{path::PathBuf, process::Command, time::Duration};

#[test]
#[ignore = "requires the registered XPC broker, built SDL3 hook, and SDL3 development files"]
fn guest_receives_virtual_controller_and_returns_feedback() {
    let root = PathBuf::from(env!("CARGO_MANIFEST_DIR"));
    let hook = root
        .join("../../../target/debug/libpolymer_hook_sdl3.dylib")
        .canonicalize()
        .unwrap();
    let runtime = std::env::var("POLYMER_SDL3_RUNTIME")
        .expect("set POLYMER_SDL3_RUNTIME to the backing SDL3 dylib");
    let temporary = tempfile::tempdir().unwrap();
    let guest = temporary.path().join("guest");
    let flags = Command::new("pkg-config")
        .args(["--cflags", "--libs", "sdl3"])
        .output()
        .unwrap();
    assert!(flags.status.success());
    assert!(
        Command::new("xcrun")
            .arg("clang")
            .arg(root.join("tests/guest.c"))
            .args(String::from_utf8(flags.stdout).unwrap().split_whitespace())
            .arg("-o")
            .arg(&guest)
            .status()
            .unwrap()
            .success()
    );
    let session = format!("polymer-sdl3-test-{}", std::process::id());
    let listener = LocalSocketListener::bind(&format!("{session}-input")).unwrap();
    let mut child = Command::new(guest)
        .env("SDL3_DYNAMIC_API", hook)
        .env("POLYMER_SDL3_RUNTIME", runtime)
        .env("POLYMER_SESSION", &session)
        .spawn()
        .unwrap();
    let mut socket = listener.accept_timeout(Duration::from_secs(10)).unwrap();
    socket
        .set_read_timeout(Some(Duration::from_secs(10)))
        .unwrap();
    let mut publisher = InputPublisher::new().unwrap();
    let Message::Hello(hello) = socket.receive().unwrap() else {
        panic!("expected input hello");
    };
    socket
        .send(Message::MapOffer(publisher.map_offer(&hello).unwrap()))
        .unwrap();
    assert_eq!(socket.receive().unwrap(), Message::MapAccepted);
    let topology = InputTopology {
        revision: 1,
        ports: vec![PortBinding {
            player: 0,
            keyboard_mouse: false,
            group: "wii".into(),
            port: 0,
            layout: "WII_REMOTE_CONTROLLER".into(),
            mode: RemapMode::Passthrough,
            bindings: vec![],
            pointer: Default::default(),
        }],
        ..Default::default()
    };
    socket
        .send(Message::Blob {
            kind: BlobKind::Topology,
            data: bincode::serde::encode_to_vec(&topology, bincode::config::standard()).unwrap(),
        })
        .unwrap();
    publisher
        .publish(InputMessage::DeviceConnected(WireDeviceConnected {
            index: 0,
            capabilities: 0,
        }))
        .unwrap();
    publisher
        .publish(InputMessage::DeviceState(WireDeviceState {
            index: 0,
            sequence: 1,
            buttons: u32::MAX,
            pressure_mask: 0,
            pressure_values: [0; 21],
            left_stick: [0; 2],
            right_stick: [0; 2],
            left_trigger: 0,
            right_trigger: 0,
            pointer_position: [0; 2],
            pointer_delta: [0; 2],
            pointer_flags: 0,
            pointer_kind: 0,
            accelerometer: [0.0; 3],
            gyroscope: [0.0; 3],
            orientation: [0.0; 4],
            has_orientation: false,
            sampled_at_ns: 0,
            battery_level: None,
            charging: false,
        }))
        .unwrap();
    for _ in 0..100 {
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
            delta: [0.5, -0.5],
            buttons: 0,
            wheel_horizontal: 0,
            wheel_vertical: 0,
            timestamp_ms: 0,
        };
        publisher
            .publish(InputMessage::HostPointer(motion))
            .unwrap();
        publisher
            .publish(InputMessage::HostPointer(WireHostPointer {
                surface_id: 1,
                coordinate_width: 1280,
                coordinate_height: 960,
                position: [0.5; 2],
                delta: [0.0; 2],
                ..motion
            }))
            .unwrap();
        std::thread::sleep(Duration::from_millis(5));
    }
    let Message::GamepadFeedback(feedback) = socket.receive().unwrap() else {
        panic!("missing rumble")
    };
    assert_eq!(
        (
            feedback.player,
            feedback.low_frequency,
            feedback.high_frequency
        ),
        (0, 123, 456)
    );
    assert!(child.wait().unwrap().success());
    listener.close();
}
