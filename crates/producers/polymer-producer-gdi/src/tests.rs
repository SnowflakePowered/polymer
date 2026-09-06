use super::*;
use polymer_ipc::{LocalSocketListener, Message, frame::FrameSubscriber};
use polymer_presentation::producer::CaptureSession;
use polymer_protocol::{Handshake, PROTOCOL_VERSION};
use windows::Win32::Foundation::{CloseHandle, HANDLE};

#[test]
fn native_paint_lease_returns_dc_and_blocks_until_consumer_release() {
    let name = format!("polymer-gdi-test-{}", std::process::id());
    let old_session = std::env::var_os(polymer_hook::config::SESSION);
    unsafe { std::env::set_var(polymer_hook::config::SESSION, &name) };
    let listener = LocalSocketListener::bind(&name).unwrap();
    let (sender, receiver) = std::sync::mpsc::channel();
    let viewer = std::thread::spawn(move || {
        // Unsupported consumers and protocol mismatch are explicit parked
        // outcomes before GDI allocates or exports a shared surface.
        for (version, api) in [
            (PROTOCOL_VERSION, ConsumerApi::Vulkan),
            (PROTOCOL_VERSION + 1, ConsumerApi::D3d12),
        ] {
            let mut socket = listener.accept().unwrap();
            socket
                .send(Message::Hello(Handshake {
                    protocol_version: version,
                    process_id: std::process::id(),
                    preferred_consumer: api,
                }))
                .unwrap();
            if version == PROTOCOL_VERSION {
                let Message::MapOffer(offer) = socket.receive().unwrap() else {
                    panic!("missing negotiated frame channel");
                };
                let _frames = FrameSubscriber::from_offer(offer).unwrap();
            }
            assert!(socket.receive().is_err());
        }
        let mut socket = listener.accept().unwrap();
        socket
            .send(Message::Hello(Handshake {
                protocol_version: PROTOCOL_VERSION,
                process_id: std::process::id(),
                preferred_consumer: ConsumerApi::D3d12,
            }))
            .unwrap();
        let Message::MapOffer(offer) = socket.receive().unwrap() else {
            panic!("missing frame channel");
        };
        let mut frames = FrameSubscriber::from_offer(offer).unwrap();
        let Message::StreamReady(ready) = socket.receive().unwrap() else {
            panic!("missing stream");
        };
        for handle in [
            ready.image_handles[0],
            ready.timeline_semaphore_handle,
            ready.consumer_semaphore_handle,
        ] {
            unsafe { CloseHandle(HANDLE(handle as *mut _)).unwrap() };
        }
        socket.send(Message::StreamAccepted).unwrap();
        for _ in 0..2 {
            assert!(matches!(socket.receive().unwrap(), Message::FrameAvailable));
            sender.send(frames.latest().unwrap().unwrap()).unwrap();
        }
        assert!(
            socket.receive().is_err(),
            "destroyed window published another frame"
        );
    });
    for _ in 0..2 {
        assert!(unsafe { Capture::new(1, 32, 24) }.unwrap().is_none());
    }
    let native = unsafe { Capture::new(1, 32, 24) }.unwrap().unwrap();
    let mut capture = CaptureSession::<Capture, (isize, u32, u32)>::from_backend(native);
    let begin = || paint::BeginPaint {
        dirty: [0, 0, 32, 24],
    };
    let dc = unsafe { capture.capture(begin()) }.unwrap().unwrap();
    assert_eq!(
        unsafe { capture.capture(paint::EndPaint { hwnd: 2, dc }) }.unwrap(),
        None
    );
    assert!(capture.as_ref().unwrap().active_paint.is_some());
    // An abandoned paint releases its native DC without publishing a frame.
    drop(capture.as_mut().unwrap().active_paint.take());
    let dc = unsafe { capture.capture(begin()) }.unwrap().unwrap();
    capture
        .as_mut()
        .unwrap()
        .active_paint
        .as_mut()
        .unwrap()
        .lease_mut()
        .release_failures = 2;
    assert!(unsafe { capture.capture(paint::EndPaint { hwnd: 1, dc }) }.is_err());
    assert!(capture.as_ref().unwrap().pending_release.is_some());
    assert_eq!(capture.as_ref().unwrap().pending_signal, None);
    assert_eq!(capture.as_ref().unwrap().submitted_value, 0);
    std::thread::scope(|scope| {
        let capture = &mut capture;
        scope
            .spawn(move || {
                assert_eq!(capture.as_mut().unwrap().recover_paint().unwrap(), false);
            })
            .join()
            .unwrap();
    });
    assert!(unsafe { capture.capture(begin()) }.is_err());
    assert!(capture.as_ref().unwrap().pending_release.is_some());
    // Recovery releases the DC and fences its unpublished work. A later paint
    // can start only once producer completion makes the surface reusable.
    let deadline = Instant::now() + Duration::from_secs(2);
    let dc = loop {
        if let Some(dc) = unsafe { capture.capture(begin()) }.unwrap() {
            break dc;
        }
        unsafe { capture.as_ref().unwrap().context.Flush() };
        assert!(
            Instant::now() < deadline,
            "failed DC release did not recover"
        );
        std::thread::yield_now();
    };
    unsafe { PatBlt(HDC(dc as *mut _), 0, 0, 32, 24, BLACKNESS) };
    assert_eq!(
        unsafe { capture.capture(paint::EndPaint { hwnd: 1, dc }) }.unwrap(),
        Some(())
    );
    let frame = receiver.recv_timeout(Duration::from_secs(2)).unwrap();
    assert_eq!(
        (
            frame.frame_number,
            frame.sync_value,
            frame.render_width,
            frame.render_height
        ),
        (1, 2, 32, 24)
    );
    assert_eq!(unsafe { capture.capture(begin()) }.unwrap(), None);
    let native = capture.as_ref().unwrap();
    unsafe {
        native
            .context
            .Signal(native.ring.consumer_fence().unwrap(), frame.sync_value)
            .unwrap();
        native.context.Flush();
    }
    let deadline = Instant::now() + Duration::from_secs(2);
    let dc = loop {
        if let Some(dc) = unsafe { capture.capture(begin()) }.unwrap() {
            break dc;
        }
        assert!(
            Instant::now() < deadline,
            "GDI slot did not become available"
        );
        std::thread::yield_now();
    };
    unsafe { capture.capture(paint::EndPaint { hwnd: 1, dc }) }.unwrap();
    let frame = receiver.recv_timeout(Duration::from_secs(2)).unwrap();
    assert_eq!((frame.frame_number, frame.sync_value), (2, 3));
    let native = capture.as_ref().unwrap();
    unsafe {
        native
            .context
            .Signal(native.ring.consumer_fence().unwrap(), frame.sync_value)
            .unwrap();
        native.context.Flush();
    }
    let deadline = Instant::now() + Duration::from_secs(2);
    let dc = loop {
        if let Some(dc) = unsafe { capture.capture(begin()) }.unwrap() {
            break dc;
        }
        assert!(Instant::now() < deadline);
        std::thread::yield_now();
    };
    let mut state = State {
        capture,
        destroyed: false,
    };
    state.window_destroyed(2);
    assert!(!state.destroyed);
    state.window_destroyed(1);
    assert!(state.destroyed);
    assert!(state.capture.as_ref().unwrap().active_paint.is_some());
    // Window destruction disconnects the viewer, but the caller can finish
    // using its redirected DC before the native surface is released.
    unsafe { PatBlt(HDC(dc as *mut _), 0, 0, 32, 24, BLACKNESS) };
    unsafe { state.capture.capture(paint::EndPaint { hwnd: 1, dc }) }.unwrap();
    state.retire_destroyed();
    assert!(state.capture.as_ref().is_none());
    assert!(!state.destroyed);
    viewer.join().unwrap();
    state.capture.configure((1, 32, 24));
    state.capture.park();
    state.window_destroyed(2);
    assert!(state.capture.is_parked());
    state.window_destroyed(1);
    assert!(
        !state.capture.is_parked(),
        "reused HWND inherited a destroyed stream's rejection"
    );
    unsafe {
        if let Some(value) = old_session {
            std::env::set_var(polymer_hook::config::SESSION, value);
        } else {
            std::env::remove_var(polymer_hook::config::SESSION);
        }
    }
}
