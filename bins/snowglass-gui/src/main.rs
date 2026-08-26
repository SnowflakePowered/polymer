#![cfg_attr(not(debug_assertions), windows_subsystem = "windows")]

mod app;
mod cli;
mod commands;
mod game_bar_watcher;
mod game_host;
mod input;
mod inspector;
mod instance_events;
mod launch;
mod metrics;
mod shell_compositor;
mod shell_document;
mod shell_dom;
mod shell_runtime;
mod theme;
mod window_manager;

#[tokio::main(flavor = "current_thread")]
async fn main() -> Result<(), Box<dyn std::error::Error>> {
    use std::time::Duration;
    use window_manager::{
        EventLoop, EventLoopExtPumpEvents, PumpStatus, TimerResolution, WinRtApartment,
    };

    let _winrt_apartment = WinRtApartment::initialize()?;

    // The generated frontend bundle has enough nested module code to exceed
    // V8's conservative default JavaScript stack during evaluation.
    // Keep this below the executable's 8 MiB stack while giving Deno's
    // primordials, the shell document, and fresh plugin isolates enough room.
    let v8_program = "snowglass-gui".to_owned();
    let unknown_v8_flags =
        deno_core::v8_set_flags(vec![v8_program.clone(), "--stack-size=4096".to_owned()]);
    debug_assert_eq!(unknown_v8_flags, [v8_program], "unknown V8 flags");

    let filter = tracing_subscriber::EnvFilter::try_from_default_env().unwrap_or_else(|_| {
        tracing_subscriber::EnvFilter::new("info,wgpu=warn,wgpu_core=warn,wgpu_hal=warn,naga=warn")
    });
    tracing_subscriber::fmt().with_env_filter(filter).init();
    let _game_bar_watcher = match game_bar_watcher::GameBarWatcher::launch() {
        Ok(watcher) => Some(watcher),
        Err(error) => {
            tracing::warn!(%error, "snowglass: could not disable the Game Bar controller shortcut");
            None
        }
    };
    let statistics_recorder = snowglass_statistics::install()?;
    let options = cli::parse()?;
    // Prepare native renderers before the shell document starts.
    let shell_rasterizer = shell_compositor::ShellRasterizer::new().await?;
    let _timer_resolution = TimerResolution::raise();
    let mut event_loop = EventLoop::<app::UserEvent>::with_user_event().build()?;
    let proxy = event_loop.create_proxy();
    let shell_factory = shell_dom::ShellSessionFactory::new(options.plugins.clone(), proxy.clone());
    tracing::debug!("snowglass: shell renderers are ready");
    let mut application = Box::new(window_manager::EventLoopApplication::new(
        app::ShellApplication::new(
            proxy,
            options,
            statistics_recorder,
            shell_factory,
            shell_rasterizer,
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
                // Winit's pumped-event extension can return immediately after
                // `AboutToWait` even when that callback selects a future
                // `WaitUntil` deadline. A plain Tokio yield therefore spins
                // this outer loop tens of thousands of times per second while
                // idle. The process raises Windows timer resolution above, so
                // a one-millisecond quantum preserves low-latency input and
                // capture delivery without burning a core between messages.
                tokio::time::sleep(Duration::from_millis(1)).await;
            }
            application.into_result()
        })
        .await
}
