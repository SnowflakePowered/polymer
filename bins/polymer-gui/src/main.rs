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

fn main() -> Result<(), Box<dyn std::error::Error>> {
    let runtime = tokio::runtime::Builder::new_multi_thread()
        .worker_threads(2)
        .enable_all()
        .on_thread_start(window_manager::WorkerWinRtApartment::initialize_current_thread)
        .on_thread_stop(window_manager::WorkerWinRtApartment::uninitialize_current_thread)
        .build()?;
    let runtime = std::sync::Arc::new(runtime);
    let system = actix::System::with_tokio_rt(|| runtime.clone());
    run(&runtime, &system)
}

fn run(
    runtime: &tokio::runtime::Runtime,
    system: &actix::SystemRunner,
) -> Result<(), Box<dyn std::error::Error>> {
    use std::time::Duration;
    use window_manager::{
        EventLoop, EventLoopExtPumpEvents, PumpStatus, TimerResolution, WinRtApartment,
    };

    let _winrt_apartment = WinRtApartment::initialize()?;

    // The generated frontend bundle has enough nested module code to exceed
    // V8's conservative default JavaScript stack during evaluation.
    // Keep this below the executable's 8 MiB stack while giving Deno's
    // primordials, the shell document, and fresh plugin isolates enough room.
    let v8_program = "polymer-gui".to_owned();
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
            tracing::warn!(%error, "polymer: could not disable the Game Bar controller shortcut");
            None
        }
    };
    let statistics_recorder = polymer_statistics::install()?;
    let options = runtime.block_on(cli::parse())?;
    // Prepare native renderers before the shell document starts.
    let shell_rasterizer = runtime.block_on(shell_compositor::ShellRasterizer::new())?;
    let _timer_resolution = TimerResolution::raise();
    let mut event_loop = EventLoop::<app::UserEvent>::with_user_event().build()?;
    let proxy = event_loop.create_proxy();
    let shell_factory = shell_dom::ShellSessionFactory::new(
        options.plugins.clone(),
        options.directories.clone(),
        options.local_storage.clone(),
        options.library.clone(),
        proxy.clone(),
    );
    tracing::debug!("polymer: shell renderers are ready");
    let mut application = Box::new(window_manager::EventLoopApplication::new(
        app::ShellApplication::new(
            proxy,
            options,
            statistics_recorder,
            shell_factory,
            shell_rasterizer,
        ),
    ));
    loop {
        application.wait_for_events()?;
        let status = system.block_on(async {
            let status = event_loop.pump_app_events(Some(Duration::ZERO), application.as_mut());
            // Give local actors a turn after native callbacks release the DOM.
            tokio::task::yield_now().await;
            status
        });
        if matches!(status, PumpStatus::Exit(_)) {
            break;
        }

        runtime.block_on(application.run_async_tasks());
    }
    runtime.block_on(application.into_result())
}
