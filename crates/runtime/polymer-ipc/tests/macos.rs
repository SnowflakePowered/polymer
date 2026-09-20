#![cfg(target_os = "macos")]
use polymer_ipc::{
    BlobKind, LocalSocketListener, Message, WireFeedback,
    input::{InputMessage, InputPublisher, InputSubscriber, WireDeviceConnected},
    xpc::{Connection, Listener, Packet},
};
use std::{
    process::Command,
    time::{Duration, Instant},
};

#[test]
#[ignore = "requires the per-user org.polymer.ipc broker"]
fn input_ring_and_feedback_cross_processes() {
    let name = format!("polymer-input-test-{}", std::process::id());
    let listener = LocalSocketListener::bind(&name).unwrap();
    let mut child = Command::new(std::env::current_exe().unwrap())
        .args(["--exact", "input_peer", "--ignored", "--nocapture"])
        .env("POLYMER_TEST_ENDPOINT", &name)
        .spawn()
        .unwrap();
    let mut socket = listener.accept_timeout(Duration::from_secs(10)).unwrap();
    socket
        .set_read_timeout(Some(Duration::from_secs(10)))
        .unwrap();
    let mut input = InputPublisher::new().unwrap();
    assert_eq!(input.accept(&mut socket).unwrap(), child.id());
    input
        .publish(InputMessage::DeviceConnected(WireDeviceConnected {
            index: 2,
            capabilities: 0x1234,
        }))
        .unwrap();
    socket
        .send_blob(BlobKind::Topology, b"topology snapshot")
        .unwrap();
    assert_eq!(
        socket.receive().unwrap(),
        Message::GamepadFeedback(WireFeedback {
            player: 2,
            low_frequency: 123,
            high_frequency: 456,
            duration_ms: 70,
        })
    );
    socket.shutdown().unwrap();
    assert!(child.wait().unwrap().success());
    listener.close();
}

#[test]
#[ignore = "subprocess entry point"]
fn input_peer() {
    let name = std::env::var("POLYMER_TEST_ENDPOINT").expect("run through parent test");
    let mut input = InputSubscriber::connect(&name).unwrap();
    input
        .control
        .set_read_timeout(Some(Duration::from_secs(10)))
        .unwrap();
    let Message::Blob {
        kind: BlobKind::Topology,
        len,
    } = input.control.receive().unwrap()
    else {
        panic!("missing topology")
    };
    assert_eq!(
        input.control.receive_blob(len).unwrap(),
        b"topology snapshot"
    );
    let deadline = Instant::now() + Duration::from_secs(5);
    let mut connected = false;
    while !connected && Instant::now() < deadline {
        input
            .drain(|message| {
                if let InputMessage::DeviceConnected(value) = message {
                    assert_eq!(
                        value,
                        WireDeviceConnected {
                            index: 2,
                            capabilities: 0x1234
                        }
                    );
                    connected = true;
                }
            })
            .unwrap();
        std::thread::yield_now();
    }
    assert!(connected, "shared input mapping did not deliver input");
    input
        .control
        .send(Message::GamepadFeedback(WireFeedback {
            player: 2,
            low_frequency: 123,
            high_frequency: 456,
            duration_ms: 70,
        }))
        .unwrap();
    assert!(
        input.control.receive().is_err(),
        "host disconnect must wake the receiver"
    );
}

#[test]
#[ignore = "requires the per-user org.polymer.ipc broker and Metal"]
fn metal_resources_cross_processes() {
    use objc2::AnyThread;
    use objc2_foundation::{NSDictionary, NSNumber, NSString};
    use objc2_io_surface::{IOSurface, IOSurfaceLockOptions};
    use objc2_metal::{
        MTLCommandBuffer, MTLCommandQueue, MTLCreateSystemDefaultDevice, MTLDevice, MTLSharedEvent,
    };
    let name = format!("polymer-metal-test-{}", std::process::id());
    let listener = Listener::bind(&name).unwrap();
    let mut child = Command::new(std::env::current_exe().unwrap())
        .args(["--exact", "metal_peer", "--ignored", "--nocapture"])
        .env("POLYMER_TEST_ENDPOINT", &name)
        .spawn()
        .unwrap();
    let connection = listener.accept_timeout(Duration::from_secs(10)).unwrap();
    connection.receive(Some(Duration::from_secs(10))).unwrap();
    let keys = [
        "IOSurfaceWidth",
        "IOSurfaceHeight",
        "IOSurfaceBytesPerElement",
        "IOSurfaceBytesPerRow",
        "IOSurfaceAllocSize",
    ]
    .map(NSString::from_str);
    let values = [4u32, 4, 4, 16, 64].map(NSNumber::new_u32);
    let properties = NSDictionary::from_slices(
        &keys.iter().map(|s| &**s).collect::<Vec<_>>(),
        &values
            .iter()
            .map(|v| &**v as &objc2::runtime::AnyObject)
            .collect::<Vec<_>>(),
    );
    let surface = IOSurface::initWithProperties(IOSurface::alloc(), &properties).unwrap();
    assert_eq!(
        surface.lockWithOptions_seed(IOSurfaceLockOptions::empty(), std::ptr::null_mut()),
        0
    );
    unsafe {
        surface
            .baseAddress()
            .cast::<u32>()
            .as_ptr()
            .write(0x1234abcd);
    }
    surface.unlockWithOptions_seed(IOSurfaceLockOptions::empty(), std::ptr::null_mut());
    let device = MTLCreateSystemDefaultDevice().unwrap();
    let event = device.newSharedEvent().unwrap();
    let consumed = device.newSharedEvent().unwrap();
    connection
        .send(
            Packet {
                surfaces: vec![surface],
                event: Some(event.newSharedEventHandle()),
                consumer_event: Some(consumed.newSharedEventHandle()),
                ..Default::default()
            },
            None,
        )
        .unwrap();
    let queue = device.newCommandQueue().unwrap();
    let commands = queue.commandBuffer().unwrap();
    commands.encodeSignalEvent_value(objc2::runtime::ProtocolObject::from_ref(&*event), 7);
    commands.commit();
    assert!(consumed.waitUntilSignaledValue_timeoutMS(9, 5000));
    assert!(child.wait().unwrap().success());
}

#[test]
#[ignore = "subprocess entry point"]
fn metal_peer() {
    use objc2_io_surface::IOSurfaceLockOptions;
    use objc2_metal::{
        MTLCommandBuffer, MTLCommandQueue, MTLCreateSystemDefaultDevice, MTLDevice, MTLSharedEvent,
    };
    let connection = Connection::connect(&std::env::var("POLYMER_TEST_ENDPOINT").unwrap()).unwrap();
    connection.send(Packet::default(), None).unwrap();
    let packet = connection.receive(Some(Duration::from_secs(10))).unwrap();
    let device = MTLCreateSystemDefaultDevice().unwrap();
    let event = device
        .newSharedEventWithHandle(packet.event.as_ref().unwrap())
        .unwrap();
    assert!(event.waitUntilSignaledValue_timeoutMS(7, 5000));
    let surface = &packet.surfaces[0];
    assert_eq!(
        surface.lockWithOptions_seed(IOSurfaceLockOptions::ReadOnly, std::ptr::null_mut()),
        0
    );
    assert_eq!(
        unsafe { surface.baseAddress().cast::<u32>().as_ptr().read() },
        0x1234abcd
    );
    surface.unlockWithOptions_seed(IOSurfaceLockOptions::ReadOnly, std::ptr::null_mut());
    let consumed = device
        .newSharedEventWithHandle(packet.consumer_event.as_ref().unwrap())
        .unwrap();
    let queue = device.newCommandQueue().unwrap();
    let commands = queue.commandBuffer().unwrap();
    commands.encodeSignalEvent_value(objc2::runtime::ProtocolObject::from_ref(&*consumed), 9);
    commands.commit();
    commands.waitUntilCompleted();
}

#[test]
#[ignore = "requires the per-user org.polymer.ipc broker"]
fn listener_close_releases_registration_and_wakes_accept() {
    let name = format!("polymer-listener-test-{}", std::process::id());
    let listener = Listener::bind(&name).unwrap();
    assert!(
        matches!(Listener::bind(&name), Err(error) if error.kind() == std::io::ErrorKind::AddrInUse)
    );
    let waiting = listener.clone();
    let worker = std::thread::spawn(move || waiting.accept_timeout(Duration::from_secs(5)));
    listener.close();
    assert!(
        matches!(worker.join().unwrap(), Err(error) if error.kind() == std::io::ErrorKind::UnexpectedEof)
    );
    // Broker invalidation is asynchronous; the name must become available
    // without dropping the still-retained, explicitly closed listener.
    let deadline = Instant::now() + Duration::from_secs(5);
    loop {
        match Listener::bind(&name) {
            Ok(replacement) => {
                replacement.close();
                break;
            }
            Err(error)
                if error.kind() == std::io::ErrorKind::AddrInUse && Instant::now() < deadline =>
            {
                std::thread::sleep(Duration::from_millis(10));
            }
            Err(error) => panic!("listener registration was not released: {error}"),
        }
    }
}
