use polymer_ipc::{LocalSocketListener, Message, frame::FrameSubscriber};
use polymer_protocol::{ConsumerApi, Handshake, PROTOCOL_VERSION};
use std::time::Duration;
use windows_sys::Win32::{
    Foundation::CloseHandle,
    Graphics::Gdi::{BLACKNESS, BeginPaint, EndPaint, InvalidateRect, PAINTSTRUCT, PatBlt},
    System::LibraryLoader::GetModuleHandleW,
    UI::WindowsAndMessaging::{CreateWindowExW, DestroyWindow, WS_OVERLAPPEDWINDOW},
};

#[test]
fn installed_destroy_window_disconnects_a_native_paint_stream() {
    let session = format!("polymer-gdi-hook-test-{}", std::process::id());
    let previous = std::env::var_os(polymer_hook::config::SESSION);
    unsafe { std::env::set_var(polymer_hook::config::SESSION, &session) };
    let listener = LocalSocketListener::bind(&session).unwrap();
    let (sender, receiver) = std::sync::mpsc::channel();
    let (probe, probes) = std::sync::mpsc::channel();
    let (checked, checks) = std::sync::mpsc::channel();
    let viewer = std::thread::spawn(move || {
        let mut socket = listener.accept().unwrap();
        socket
            .set_read_timeout(Some(Duration::from_secs(3)))
            .unwrap();
        socket
            .send(Message::Hello(Handshake {
                protocol_version: PROTOCOL_VERSION,
                process_id: std::process::id(),
                preferred_consumer: ConsumerApi::D3d12,
            }))
            .unwrap();
        let Message::MapOffer(offer) = socket.receive().unwrap() else {
            panic!("missing frame channel")
        };
        let mut frames = FrameSubscriber::from_offer(offer).unwrap();
        let Message::StreamReady(ready) = socket.receive().unwrap() else {
            panic!("missing native GDI stream")
        };
        // This test checks installed hook routing and stream lifetime; the
        // producer's native fixture covers the surface lease and synchronization.
        for handle in ready.image_handles[..ready.slot_count as usize]
            .iter()
            .copied()
            .chain([
                ready.timeline_semaphore_handle,
                ready.consumer_semaphore_handle,
            ])
            .filter(|handle| *handle != 0)
        {
            assert_ne!(unsafe { CloseHandle(handle as *mut _) }, 0);
        }
        socket.send(Message::StreamAccepted).unwrap();
        assert!(matches!(socket.receive().unwrap(), Message::FrameAvailable));
        let frame = frames.latest().unwrap().unwrap();
        sender.send((ready.source_window, frame)).unwrap();
        probes.recv_timeout(Duration::from_secs(3)).unwrap();
        assert!(
            !socket.peer_disconnected().unwrap(),
            "failed native destruction must retain the stream"
        );
        checked.send(()).unwrap();
        let error = socket.receive().unwrap_err();
        assert_eq!(
            error.kind(),
            std::io::ErrorKind::UnexpectedEof,
            "successful native window destruction must close the stream"
        );
    });
    unsafe {
        assert_eq!(crate::install::install(), 0);
        let class = "STATIC\0".encode_utf16().collect::<Vec<_>>();
        let window = CreateWindowExW(
            0,
            class.as_ptr(),
            class.as_ptr(),
            WS_OVERLAPPEDWINDOW,
            0,
            0,
            96,
            96,
            std::ptr::null_mut(),
            std::ptr::null_mut(),
            GetModuleHandleW(std::ptr::null()),
            std::ptr::null(),
        );
        assert!(!window.is_null());
        assert_ne!(InvalidateRect(window, std::ptr::null(), 0), 0);
        let mut paint = PAINTSTRUCT::default();
        let dc = BeginPaint(window, &mut paint);
        assert!(!dc.is_null());
        assert_ne!(PatBlt(dc, 0, 0, 16, 16, BLACKNESS), 0);
        assert_ne!(EndPaint(window, &paint), 0);
        let (source, frame) = receiver.recv_timeout(Duration::from_secs(3)).unwrap();
        assert_eq!(source, window as i64);
        assert_eq!(frame.frame_number, 1);
        assert!(frame.render_width > 0 && frame.render_height > 0);
        // Win32 rejects destruction by a thread that does not own the window.
        let identity = window as usize;
        assert_eq!(
            std::thread::spawn(move || DestroyWindow(identity as *mut _))
                .join()
                .unwrap(),
            0
        );
        probe.send(()).unwrap();
        checks.recv_timeout(Duration::from_secs(3)).unwrap();
        assert_ne!(DestroyWindow(window), 0);
    }
    viewer.join().unwrap();
    unsafe {
        if let Some(previous) = previous {
            std::env::set_var(polymer_hook::config::SESSION, previous);
        } else {
            std::env::remove_var(polymer_hook::config::SESSION);
        }
    }
}
