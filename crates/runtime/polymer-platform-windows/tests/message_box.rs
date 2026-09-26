#![cfg(windows)]
use polymer_automation::{AutomationDriver, AutomationRequest, WindowQuery};
use polymer_platform_windows::WindowsAutomationDriver;
use std::{
    process::{Child, Command},
    time::Duration,
};

const FIXTURE: &str = r#"
Add-Type -AssemblyName System.Windows.Forms
$form = New-Object System.Windows.Forms.Form
$form.Text = 'Polymer message box fixture'
$timer = New-Object System.Windows.Forms.Timer
$timer.Interval = 50
$timer.Add_Tick({
    if (Test-Path $env:POLYMER_MESSAGE_BOX_TRIGGER) {
        $timer.Stop()
        [System.Windows.Forms.MessageBox]::Show($form, 'Guest failed', 'Polymer fixture alert') | Out-Null
    }
})
$timer.Start()
[System.Windows.Forms.Application]::Run($form)
"#;

struct Guest(Child);
impl Drop for Guest {
    fn drop(&mut self) {
        let _ = self.0.kill();
        let _ = self.0.wait();
    }
}

#[test]
#[ignore = "requires an interactive desktop"]
fn message_box_watch_reports_an_owned_message_box() {
    let directory = tempfile::tempdir().unwrap();
    let trigger = directory.path().join("show");
    let guest = Guest(
        Command::new("powershell")
            .args(["-NoProfile", "-NonInteractive", "-Command", FIXTURE])
            .env("POLYMER_MESSAGE_BOX_TRIGGER", &trigger)
            .spawn()
            .unwrap(),
    );
    let mut driver = WindowsAutomationDriver::new(guest.0.id(), None).unwrap();
    driver
        .execute(AutomationRequest::WaitForWindow {
            query: WindowQuery {
                pattern: Some("^Polymer message box fixture$".into()),
                selector: None,
            },
            timeout_ms: 15_000,
        })
        .unwrap();
    let mut message_boxes = driver.message_boxes().unwrap();
    std::fs::write(&trigger, []).unwrap();
    let message_box = tokio::runtime::Builder::new_current_thread()
        .enable_time()
        .build()
        .unwrap()
        .block_on(async {
            tokio::time::timeout(Duration::from_secs(10), message_boxes.next()).await
        })
        .expect("no message box was reported")
        .expect("message box watch closed");
    assert_eq!(message_box.name.as_deref(), Some("Polymer fixture alert"));
    driver.shutdown();
}
