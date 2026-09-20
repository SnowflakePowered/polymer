#![cfg(target_os = "macos")]
use polymer_automation::{
    AutomationDriver, AutomationRequest, AutomationResponse, InputCommand, TreeOptions, WindowQuery,
};
use polymer_ipc::input::host::InputIpcListener;
use polymer_platform_macos::MacAutomationDriver;
use std::{
    path::PathBuf,
    process::{Child, Command},
    time::{Duration, Instant},
};

struct Guest(Child);
impl Drop for Guest {
    fn drop(&mut self) {
        let _ = self.0.kill();
        let _ = self.0.wait();
    }
}

#[test]
#[ignore = "requires Accessibility, WindowServer, the XPC broker and POLYMER_APPKIT_HOOK"]
fn xa11y_discovers_and_clicks_an_isolated_guest_window() {
    let hook =
        PathBuf::from(std::env::var_os("POLYMER_APPKIT_HOOK").expect("set POLYMER_APPKIT_HOOK"));
    let directory = tempfile::tempdir().unwrap();
    let executable = directory.path().join("automation-fixture");
    let output = Command::new("xcrun")
        .args(["clang", "-fobjc-arc", "-framework", "Cocoa"])
        .arg(concat!(env!("CARGO_MANIFEST_DIR"), "/tests/automation.m"))
        .arg("-o")
        .arg(&executable)
        .output()
        .unwrap();
    assert!(
        output.status.success(),
        "{}",
        String::from_utf8_lossy(&output.stderr)
    );
    let session = format!("polymer-automation-test-{}", std::process::id());
    let input = InputIpcListener::bind(&session).unwrap();
    let mut guest = Guest(
        Command::new(executable)
            .env("POLYMER_SESSION", &session)
            .env("POLYMER_DESKTOP_ISOLATION", "1")
            .env("DYLD_INSERT_LIBRARIES", hook)
            .spawn()
            .unwrap(),
    );
    let mut driver = MacAutomationDriver::new(guest.0.id(), input.clone()).unwrap();
    let response = driver
        .execute(AutomationRequest::WaitForWindow {
            query: WindowQuery {
                pattern: Some("^Polymer automation fixture$".into()),
                selector: None,
            },
            timeout_ms: 5000,
        })
        .unwrap();
    let AutomationResponse::Window(window) = response else {
        panic!("expected window")
    };
    let AutomationResponse::Tree { root, .. } = driver
        .execute(AutomationRequest::Tree {
            window: window.id.clone(),
            options: TreeOptions::default(),
        })
        .unwrap()
    else {
        panic!("expected tree")
    };
    let bounds = root.bounds.unwrap();
    // The guest input handshake can finish just after the AX window appears.
    let deadline = Instant::now() + Duration::from_secs(5);
    loop {
        let result = driver.execute(AutomationRequest::Input {
            window: window.id.clone(),
            command: InputCommand::MouseClick {
                button: "left".into(),
                count: 1,
                x: (bounds.x + bounds.width / 2.) as i32,
                y: (bounds.y + bounds.height / 2.) as i32,
            },
        });
        match result {
            Ok(_) => break,
            Err(error) if error.kind == "processExited" && Instant::now() < deadline => {
                std::thread::sleep(Duration::from_millis(20))
            }
            Err(error) => panic!("{error}"),
        }
    }
    // Exercise the same native state polling that Dolphin uses for hotkeys.
    for pressed in [true, false] {
        input.publish_host_action(polymer_input::HostInputEvent::Key {
            code: polymer_input::KeyCode::new("Unidentified"),
            scan_code: 0x6d,
            virtual_key: 0x6d,
            pressed,
            repeat: false,
            extended: false,
            target_window: 0,
            timestamp_ms: 0,
            sequence: 0,
        });
        std::thread::sleep(Duration::from_millis(50));
    }
    loop {
        if let Some(status) = guest.0.try_wait().unwrap() {
            assert!(status.success(), "isolated guest did not receive click");
            break;
        }
        assert!(
            Instant::now() < deadline,
            "isolated guest did not receive click"
        );
        std::thread::sleep(Duration::from_millis(20));
    }
    driver.shutdown();
}
