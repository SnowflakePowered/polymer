mod egui;

use tauri::Manager;

fn main() {
    tauri::Builder::default()
        .setup(|app| {
            let window = app
                .get_webview_window("main")
                .ok_or("the main webview window was not created")?;
            window.set_background_color(Some(tauri::window::Color(0, 0, 0, 0)))?;
            Ok(())
        })
        .invoke_handler(tauri::generate_handler![egui::ipc::debug_snapshot])
        .run(tauri::generate_context!())
        .expect("failed to run snowglass frontend");
}
