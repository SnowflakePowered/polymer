#![cfg_attr(not(debug_assertions), windows_subsystem = "windows")]

mod app;
mod baseui;
mod cli;
mod commands;
#[cfg(windows)]
mod game_bar_watcher;
mod game_host;
mod input;
mod inspector;
mod instance_events;
mod launch;
mod metrics;
mod shell;
mod theme;
#[cfg(windows)]
mod updater;
mod window_manager;

fn main() -> Result<(), Box<dyn std::error::Error>> {
    #[cfg(windows)]
    if let Some((parent, task)) = cli::ShellArguments::elevated_task() {
        std::process::exit(task.run(parent));
    }
    #[cfg(windows)]
    updater::Updater::startup();
    let mut builder = tokio::runtime::Builder::new_multi_thread();
    builder.worker_threads(2).enable_all();
    #[cfg(windows)]
    builder
        .on_thread_start(
            polymer_composition_windows::WorkerWinRtApartment::initialize_current_thread,
        )
        .on_thread_stop(
            polymer_composition_windows::WorkerWinRtApartment::uninitialize_current_thread,
        );
    let runtime = builder.build()?;
    #[cfg(windows)]
    if baseui::Setup::required() {
        return baseui::Setup::run(&runtime);
    }
    if cli::ShellArguments::pick_theme_requested() {
        return baseui::BaseUi::run(&runtime);
    }
    let runtime = std::sync::Arc::new(runtime);
    let system = actix::System::with_tokio_rt(|| runtime.clone());
    #[cfg(windows)]
    let updater = updater::Updater::start();
    run(&runtime, &system)?;
    #[cfg(windows)]
    if let Some(updater) = updater {
        updater.apply_on_exit();
    }
    Ok(())
}

fn run(
    runtime: &tokio::runtime::Runtime,
    system: &actix::SystemRunner,
) -> Result<(), Box<dyn std::error::Error>> {
    // Native event callbacks and application teardown also spawn async work.
    // Keep their runtime context alive beyond the individual block_on turns.
    let _runtime_context = runtime.enter();
    use window_manager::EventLoop;
    #[cfg(windows)]
    use window_manager::{EventLoopExtPumpEvents, PumpStatus};

    #[cfg(windows)]
    use polymer_composition_windows::{TimerResolution, WinRtApartment};
    #[cfg(windows)]
    let _winrt_apartment = WinRtApartment::initialize()?;

    // The generated frontend bundle has enough nested module code to exceed
    // V8's conservative default JavaScript stack during evaluation.
    // Keep this below the shell thread's 8 MiB stack while giving Deno's
    // primordials, the shell document, and fresh plugin isolates enough room.
    let v8_program = "polymer-gui".to_owned();
    let unknown_v8_flags =
        deno_core::v8_set_flags(vec![v8_program.clone(), "--stack-size=4096".to_owned()]);
    debug_assert_eq!(unknown_v8_flags, [v8_program], "unknown V8 flags");

    let filter = tracing_subscriber::EnvFilter::try_from_default_env().unwrap_or_else(|_| {
        tracing_subscriber::EnvFilter::new("info,wgpu=warn,wgpu_core=warn,wgpu_hal=warn,naga=warn")
    });
    tracing_subscriber::fmt().with_env_filter(filter).init();
    #[cfg(windows)]
    let _instance_mutex = unsafe {
        use windows::Win32::Foundation::{ERROR_ALREADY_EXISTS, GetLastError};
        let mutex = windows::Win32::System::Threading::CreateMutexW(
            None,
            false,
            windows::core::w!("Local\\Polymer.Instance"),
        )?;
        if GetLastError() == ERROR_ALREADY_EXISTS {
            return Err("Polymer is already running".into());
        }
        mutex
    };
    let _instance_lock = polymer_library::Directories::discover()?.lock().map_err(
        |error| -> Box<dyn std::error::Error> {
            match error.kind() {
                std::io::ErrorKind::WouldBlock => "Polymer is already running".into(),
                _ => error.into(),
            }
        },
    )?;
    #[cfg(windows)]
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
    #[cfg(windows)]
    let compositor_factory =
        runtime.block_on(polymer_composition_windows::WindowsCompositor::prepare())?;
    #[cfg(target_os = "macos")]
    let compositor_factory =
        runtime.block_on(polymer_composition_macos::MacosCompositor::prepare())?;
    #[cfg(windows)]
    let _timer_resolution = TimerResolution::raise();
    let event_loop = EventLoop::<app::UserEvent>::with_user_event().build()?;
    #[cfg(windows)]
    let mut event_loop = event_loop;
    let proxy = event_loop.create_proxy();
    let shell_factory = shell::ShellSessionFactory::new(
        options.debug,
        options.plugins.clone(),
        options.directories.clone(),
        options.local_storage.clone(),
        options.library.clone(),
        options.jobs.clone(),
        proxy.clone(),
    );
    tracing::debug!("polymer: shell renderers are ready");
    let mut application = system.block_on(async {
        Box::new(window_manager::EventLoopApplication::new(
            app::ShellApplication::new(
                proxy,
                options,
                statistics_recorder,
                shell_factory,
                compositor_factory,
            ),
            #[cfg(windows)]
            polymer_composition_windows::NativeEventLoop::wait_for_events,
        ))
    });
    #[cfg(target_os = "macos")]
    {
        // AppKit owns its run loop. Pumping and restarting NSApplication can
        // repeatedly fire expired run-loop timers between external turns.
        application.set_native_turn(|inner| {
            system.block_on(async { tokio::task::yield_now().await });
            runtime.block_on(inner.run_async_tasks());
        });
        event_loop.run_app(application.as_mut())?;
    }
    #[cfg(windows)]
    loop {
        application.wait_for_events()?;
        let status = system.block_on(async {
            let status =
                event_loop.pump_app_events(Some(std::time::Duration::ZERO), application.as_mut());
            // Give local actors a turn after native callbacks release the coordinator.
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
