use super::MediaSignal;
use crate::shell::protocol::{ShellCommand, ShellInbox};
#[cfg(target_os = "macos")]
use polymer_platform_macos::DefaultOutputWatcher;
#[cfg(windows)]
use polymer_platform_windows::DefaultOutputWatcher;
use rodio::{DeviceSinkBuilder, MixerDeviceSink, cpal::StreamError};
use std::{
    sync::mpsc::{self, Receiver, RecvTimeoutError, Sender},
    time::Duration,
};

const RETRY_PERIOD: Duration = Duration::from_secs(2);

enum OutputCommand {
    Reopen(u64),
    DefaultChanged,
    Shutdown,
}

pub(super) struct MediaOutput {
    commands: Sender<OutputCommand>,
}

impl MediaOutput {
    pub(super) fn spawn(shell: ShellInbox) -> Self {
        let (commands, receiver) = mpsc::channel();
        let thread = OutputThread {
            shell,
            errors: commands.clone(),
            receiver,
            generation: 0,
        };
        let spawned = std::thread::Builder::new()
            .name("polymer-media-output".into())
            .spawn(move || thread.run());
        if let Err(error) = spawned {
            tracing::warn!(%error, "polymer: starting the audio output failed");
        }
        Self { commands }
    }
}

impl Drop for MediaOutput {
    fn drop(&mut self) {
        let _ = self.commands.send(OutputCommand::Shutdown);
    }
}

struct OutputThread {
    shell: ShellInbox,
    errors: Sender<OutputCommand>,
    receiver: Receiver<OutputCommand>,
    generation: u64,
}

impl OutputThread {
    fn run(mut self) {
        let changes = self.errors.clone();
        let _watcher = DefaultOutputWatcher::new(move || {
            let _ = changes.send(OutputCommand::DefaultChanged);
        })
        .inspect_err(|error| tracing::warn!(%error, "polymer: watching the default audio output failed"))
        .ok();
        loop {
            self.generation += 1;
            let output = self.open();
            let mixer = output.as_ref().map(|output| output.mixer().clone());
            self.shell.send(ShellCommand::Media(MediaSignal::Output(mixer)));
            if !self.wait(output.is_some()) {
                return;
            }
        }
    }

    fn open(&self) -> Option<MixerDeviceSink> {
        let (errors, generation) = (self.errors.clone(), self.generation);
        let mut output = DeviceSinkBuilder::from_default_device()
            .and_then(|builder| {
                builder
                    .with_error_callback(move |error| {
                        if matches!(
                            error,
                            StreamError::DeviceNotAvailable | StreamError::StreamInvalidated
                        ) {
                            let _ = errors.send(OutputCommand::Reopen(generation));
                        }
                    })
                    .open_sink_or_fallback()
            })
            .inspect_err(|error| tracing::warn!(%error, "polymer: opening the audio output failed"))
            .ok()?;
        output.log_on_drop(false);
        Some(output)
    }

    fn wait(&self, open: bool) -> bool {
        loop {
            let command = if open {
                self.receiver
                    .recv()
                    .map_err(|_| RecvTimeoutError::Disconnected)
            } else {
                self.receiver.recv_timeout(RETRY_PERIOD)
            };
            match command {
                Ok(OutputCommand::Reopen(generation)) if generation != self.generation => {}
                Ok(OutputCommand::Reopen(_) | OutputCommand::DefaultChanged)
                | Err(RecvTimeoutError::Timeout) => return true,
                Ok(OutputCommand::Shutdown) | Err(RecvTimeoutError::Disconnected) => return false,
            }
        }
    }
}
