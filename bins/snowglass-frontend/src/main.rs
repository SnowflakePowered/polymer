mod capture;
mod egui;

use clap::{Parser, ValueEnum};
use snowglass_renderer::capture::ConsumerPreference;
use tauri::Manager;

#[derive(Clone, Copy, Debug, Default, ValueEnum)]
enum RendererArgument {
    #[default]
    D3d12,
    Vulkan,
}

impl From<RendererArgument> for ConsumerPreference {
    fn from(value: RendererArgument) -> Self {
        match value {
            RendererArgument::D3d12 => Self::D3d12,
            RendererArgument::Vulkan => Self::Vulkan,
        }
    }
}

#[derive(Debug, Parser)]
struct Options {
    /// Capture IPC session name used by injected producers.
    #[arg(long, env = "SNOWGLASS_SESSION")]
    session: Option<String>,

    #[arg(long, value_enum, default_value_t)]
    renderer: RendererArgument,
}

fn main() {
    let options = Options::parse();
    let session = options
        .session
        .unwrap_or_else(|| format!("snowglass-frontend-{}", std::process::id()));
    eprintln!("snowglass: frontend capture session is {session}");
    let preference = options.renderer.into();
    let mut lifecycle = None;

    tauri::Builder::default()
        .invoke_handler(tauri::generate_handler![egui::ipc::debug_snapshot])
        .build(tauri::generate_context!())
        .expect("failed to build snowglass frontend")
        .run(move |app, event| match event {
            tauri::RunEvent::Ready => {
                let Some(window) = app.get_webview_window("main") else {
                    eprintln!("snowglass: Tauri did not create the main window");
                    app.exit(1);
                    return;
                };
                if let Err(error) =
                    window.set_background_color(Some(tauri::window::Color(0, 0, 0, 0)))
                {
                    eprintln!("snowglass: failed to make the frontend transparent: {error}");
                }
                let wake_handle = app.clone();
                match capture::CaptureLifecycle::new(window, &session, preference, move || {
                    let _ = wake_handle.run_on_main_thread(|| {});
                }) {
                    Ok(value) => lifecycle = Some(value),
                    Err(error) => {
                        eprintln!("snowglass: failed to start capture lifecycle: {error}");
                        app.exit(1);
                    }
                }
            }
            tauri::RunEvent::MainEventsCleared => {
                if let Some(lifecycle) = lifecycle.as_mut() {
                    lifecycle.process();
                    lifecycle.draw();
                }
            }
            tauri::RunEvent::WindowEvent { label, event, .. } if label == "main" => {
                if matches!(
                    event,
                    tauri::WindowEvent::Resized(_) | tauri::WindowEvent::ScaleFactorChanged { .. }
                ) && let Some(lifecycle) = lifecycle.as_mut()
                {
                    lifecycle.resize();
                }
            }
            tauri::RunEvent::Exit => {
                if let Some(lifecycle) = lifecycle.as_mut() {
                    lifecycle.shutdown();
                }
            }
            _ => {}
        });
}
