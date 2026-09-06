#[test]
#[ignore = "requires POLYMER_APPLICATION_PATH, a rebuilt Vulkan layer, and a private POLYMER_SESSION"]
fn external_vulkan_application_publishes_importable_pixels() {
    use std::os::windows::process::CommandExt;
    assert!(std::env::var_os("POLYMER_APPLICATION_PATH").is_some());
    let mut viewer = polymer_test_viewer::WsiViewer::start(1);
    let mut application = std::process::Command::new("powershell.exe")
        .args(["-NoProfile", "-Command", r#"
Add-Type -TypeDefinition @'
using System;
using System.Runtime.InteropServices;
public static class CubePaint {
    private delegate bool Visit(IntPtr window, IntPtr data);
    [DllImport("user32.dll")] private static extern bool EnumWindows(Visit visit, IntPtr data);
    [DllImport("user32.dll")] private static extern uint GetWindowThreadProcessId(IntPtr window, out uint process);
    [DllImport("user32.dll")] private static extern bool PostMessage(IntPtr window, uint message, IntPtr wparam, IntPtr lparam);
    public static void Paint(uint process) {
        EnumWindows((window, data) => {
            uint owner; GetWindowThreadProcessId(window, out owner);
            if (owner == process) PostMessage(window, 15, IntPtr.Zero, IntPtr.Zero);
            return true;
        }, IntPtr.Zero);
    }
}
'@
$launchOptions = @{}
if ($env:POLYMER_APPLICATION_LOADER_LOG) { $launchOptions.RedirectStandardError = $env:POLYMER_APPLICATION_LOADER_LOG }
$app = Start-Process -FilePath $env:POLYMER_APPLICATION_PATH -ArgumentList @('--c','120','--width','320','--height','240','--suppress_popups') -WindowStyle Hidden -PassThru @launchOptions
$deadline = [DateTime]::UtcNow.AddSeconds(15)
$verifiedLayer = -not $env:POLYMER_APPLICATION_LAYER_PATH
while (-not $app.WaitForExit(20)) {
    if (-not $verifiedLayer) {
        $app.Refresh()
        $layer = $app.Modules | Where-Object { $_.ModuleName -eq 'polymer_hook_vulkan_layer.dll' }
        if ($layer) {
            $expected = [IO.Path]::GetFullPath($env:POLYMER_APPLICATION_LAYER_PATH)
            if ($layer.FileName -ine $expected) { $app.Kill(); throw "Unexpected capture layer: $($layer.FileName)" }
            Write-Output "Verified application layer: $($layer.FileName)"
            $verifiedLayer = $true
        }
    }
    [CubePaint]::Paint($app.Id)
    if ([DateTime]::UtcNow -gt $deadline) { $app.Kill(); exit 1 }
}
if (-not $verifiedLayer) { throw 'Capture layer module was not observed' }
exit $app.ExitCode
"#])
        .creation_flags(0x08000000)
        .spawn().unwrap();
    viewer.verify_application(polymer_hook::config::tracking_enabled());
    drop(viewer);
    assert!(application.wait().unwrap().success());
}
