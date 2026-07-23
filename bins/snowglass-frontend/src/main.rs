mod capture;
mod egui;

use clap::{ArgAction, Parser, ValueEnum};
use snowglass_renderer::capture::ConsumerPreference;
use std::{net::SocketAddr, path::PathBuf};
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
#[command(name = "snowglass-frontend", version, about)]
struct Options {
    /// Capture IPC session name used by injected producers.
    #[arg(long, env = "SNOWGLASS_SESSION")]
    session: Option<String>,

    /// Graphics API used to render the frontend window.
    #[arg(long, value_enum, default_value_t)]
    api: RendererArgument,

    #[arg(long = "gamepad", action = ArgAction::Append, value_name = "SELECTOR:SLOT:LAYOUT")]
    gamepads: Vec<String>,

    #[arg(long, value_name = "FILE")]
    stone_db: Option<PathBuf>,

    #[arg(long, value_name = "FILE")]
    gamecontroller_db: Option<PathBuf>,

    #[arg(
        long,
        visible_alias = "track-render-target",
        num_args = 0..=1,
        default_missing_value = "0",
        value_delimiter = ',',
        value_name = "INDEXES"
    )]
    track_render_targets: Option<Vec<String>>,

    #[arg(long, action = ArgAction::SetTrue, requires = "track_render_targets")]
    graph_tracking: bool,

    #[arg(long, value_name = "CSS")]
    layout_css: Option<String>,

    #[arg(
        long,
        num_args = 0..,
        default_missing_value = "0",
        value_delimiter = ',',
        value_name = "INDEXES"
    )]
    webrtc: Option<Vec<u32>>,

    #[arg(
        long,
        default_value = "127.0.0.1:8080",
        value_name = "IP:PORT",
        requires = "webrtc"
    )]
    bind_webrtc: SocketAddr,

    #[arg(
        long,
        default_value_t = 16,
        num_args = 0..=1,
        default_missing_value = "16"
    )]
    surfaces: u32,

    #[arg(
        long,
        visible_aliases = ["skip-surface", "skip-window", "skip-windows"],
        default_value_t = 0,
        num_args = 0..=1,
        default_missing_value = "1"
    )]
    skip_surfaces: u32,

    #[arg(long, action = ArgAction::SetTrue, conflicts_with = "no_isolate")]
    isolate: bool,

    #[arg(long, action = ArgAction::SetTrue)]
    no_isolate: bool,

    #[arg(long, action = ArgAction::SetTrue, conflicts_with = "no_isolate")]
    resize: bool,

    #[arg(long = "force-km")]
    force_keyed_mutex: bool,

    #[arg(
        long = "hook",
        action = ArgAction::Append,
        value_delimiter = ',',
        value_name = "HOOKS"
    )]
    hooks: Vec<String>,

    /// Target program and its arguments. Must follow `--`.
    #[arg(last = true, num_args = 0.., value_name = "COMMAND")]
    target: Vec<String>,
}

fn main() {
    let options = Options::parse();
    let session = options
        .session
        .unwrap_or_else(|| format!("snowglass-frontend-{}", std::process::id()));
    eprintln!("snowglass: frontend capture session is {session}");
    let preference = options.api.into();
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
