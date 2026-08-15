#![cfg_attr(not(debug_assertions), windows_subsystem = "windows")]

mod app;
mod background_document;
mod background_dom;
mod background_runtime;
mod cli;
mod commands;
mod foreground_dom;
mod game_host;
mod input;
mod instance_events;
mod launch;
mod launcher;
mod metrics;
mod shared_bus;
mod window_manager;

#[tokio::main(flavor = "current_thread")]
async fn main() -> Result<(), Box<dyn std::error::Error>> {
    use std::time::Duration;
    use window_manager::{EventLoop, EventLoopExtPumpEvents, PumpStatus, TimerResolution};

    // ReactDOM's production bundle has enough nested generated code to exceed
    // V8's conservative default JavaScript stack during module evaluation.
    // Keep this below Rust/Tokio's worker stack while giving both the
    // background document and fresh plugin isolates a consistent budget.
    let v8_program = "snowglass-shell-webview2".to_owned();
    let unknown_v8_flags =
        deno_core::v8_set_flags(vec![v8_program.clone(), "--stack-size=1536".to_owned()]);
    debug_assert_eq!(unknown_v8_flags, [v8_program], "unknown V8 flags");

    let filter = tracing_subscriber::EnvFilter::try_from_default_env().unwrap_or_else(|_| {
        tracing_subscriber::EnvFilter::new("info,wgpu=warn,wgpu_core=warn,wgpu_hal=warn,naga=warn")
    });
    tracing_subscriber::fmt().with_env_filter(filter).init();
    let statistics_recorder = snowglass_statistics::install()?;
    let options = cli::parse()?;
    // Prepare native chrome renderers before WebView2 starts Chromium.
    let background_rasterizer = game_host::BackgroundRasterizer::new().await?;
    let background_factory =
        background_document::BackgroundDocumentFactory::new(options.plugins.clone());
    tracing::debug!("snowglass: background renderers are ready");
    let _timer_resolution = TimerResolution::raise();
    let mut event_loop = EventLoop::<app::UserEvent>::with_user_event().build()?;
    let proxy = event_loop.create_proxy();
    let mut application = Box::new(window_manager::EventLoopApplication::new(
        app::ShellApplication::new(
            proxy,
            options,
            statistics_recorder,
            background_factory,
            background_rasterizer,
        ),
    ));
    let tasks = tokio::task::LocalSet::new();
    tasks
        .run_until(async {
            loop {
                let status = event_loop
                    .pump_app_events(Some(Duration::from_millis(100)), application.as_mut());
                if matches!(status, PumpStatus::Exit(_)) {
                    break;
                }
                application.run_async_tasks().await;
            }
            application.into_result()
        })
        .await
}
