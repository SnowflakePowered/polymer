#![cfg(target_os = "macos")]
use objc2_metal::*;
use polymer_consumer_metal::{ImportedMetalStream, ProducerTimeline};
use polymer_ipc::{LocalSocketListener, Message};
use polymer_protocol::{ConsumerApi, FrameReady, Handshake, PROTOCOL_VERSION};
use std::{path::PathBuf, process::Command, time::Duration};

#[test]
#[ignore = "requires the registered XPC broker, Metal, and the built hook dylib"]
fn injected_drawables_reach_the_shared_consumer() {
    let root = PathBuf::from(env!("CARGO_MANIFEST_DIR"));
    let workspace = root.join("../../..");
    let artifacts = workspace.join("target/debug").canonicalize().unwrap();
    let temporary = tempfile::tempdir().unwrap();
    let executable = temporary.path().join("guest");
    assert!(
        Command::new("xcrun")
            .args([
                "clang",
                "-fobjc-arc",
                "-framework",
                "Foundation",
                "-framework",
                "QuartzCore",
                "-framework",
                "Metal"
            ])
            .arg(root.join("tests/guest.m"))
            .arg("-o")
            .arg(&executable)
            .status()
            .unwrap()
            .success()
    );
    let name = format!("polymer-metal-capture-{}", std::process::id());
    let listener = LocalSocketListener::bind(&name).unwrap();
    let mut child = Command::new(executable)
        .env(
            "DYLD_INSERT_LIBRARIES",
            artifacts.join("libpolymer_hook_metal.dylib"),
        )
        .env("POLYMER_SESSION", &name)
        .env(
            "POLYMER_RENDER_CONFIGURATION",
            r#"{"surfaces":[{"index":0}]}"#,
        )
        .spawn()
        .unwrap();
    let mut control = listener.accept_timeout(Duration::from_secs(10)).unwrap();
    control
        .set_read_timeout(Some(Duration::from_secs(10)))
        .unwrap();
    control
        .send(Message::Hello(Handshake {
            protocol_version: PROTOCOL_VERSION,
            process_id: std::process::id(),
            preferred_consumer: ConsumerApi::Metal,
        }))
        .unwrap();
    let device = MTLCreateSystemDefaultDevice().unwrap();
    let queue = device.newCommandQueue().unwrap();
    let (imported, timeline) = match control.receive() {
        Ok(Message::StreamReady(stream)) => {
            let resources = control.take_stream_resources().unwrap();
            let timeline = ProducerTimeline::open(&resources).unwrap();
            let imported = ImportedMetalStream::new(&device, stream, resources).unwrap();
            control.send(Message::StreamAccepted).unwrap();
            (imported, timeline)
        }
        other => panic!("expected a stream, got {other:?}"),
    };
    let mut observed = 0;
    let mut received = 0;
    while received < 36 {
        observed = timeline
            .wait_beyond(observed)
            .unwrap_or_else(|| panic!("ready timeline closed after {received} frames"));
        let frame = FrameReady::observed(observed);
        let mut pixel = [0u8; 4];
        unsafe {
            imported
                .texture(frame)
                .unwrap()
                .getBytes_bytesPerRow_fromRegion_mipmapLevel(
                    std::ptr::NonNull::from(&mut pixel).cast(),
                    4,
                    MTLRegion {
                        origin: MTLOrigin { x: 32, y: 32, z: 0 },
                        size: MTLSize {
                            width: 1,
                            height: 1,
                            depth: 1,
                        },
                    },
                    0,
                );
        }
        assert!((pixel[0] as i16 - 128).abs() <= 1);
        assert!((pixel[1] as i16 - 64).abs() <= 1);
        assert!(
            (pixel[2] as i16 - (frame.frame_number * 255 / 64) as i16).abs() <= 1,
            "frame {:?}, pixel {:?}",
            frame,
            pixel
        );
        assert_eq!(pixel[3], 255);
        let commands = queue.commandBuffer().unwrap();
        imported.release(&commands, frame.sync_value - 1);
        commands.commit();
        commands.waitUntilCompleted();
        received = frame.frame_number;
    }
    assert_eq!(received, 36);
    assert!(child.wait().unwrap().success());
}
