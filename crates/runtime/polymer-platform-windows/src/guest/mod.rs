//! Windows implementation of the native guest session.

mod audio;
mod launch;

use self::audio::AudioWorker;
use crate::{NativeWindow, TargetProcess, client_size, keyboard_code};
use polymer_input::{HostInputEvent, KeyCode};
use polymer_ipc::{Message, input::host::InputIpcListener};
use polymer_platform::{
    AudioState, Guest, GuestInput, GuestWindowId, HotkeyChord, NormalizedVolume,
    ProcessArchitecture, ProcessExit, ProcessExitReceiver, ProcessId, ResizeDimensions,
};
use std::{
    io,
    sync::{
        atomic::{AtomicU32, Ordering},
        mpsc,
    },
    thread,
    time::{Duration, Instant},
};

/// Selected by the host at compile time. Isolation has no separate runtime protocol.
pub struct GuestPlatform;

struct WindowsGuest {
    process: Option<TargetProcess>,
    process_id: ProcessId,
    architecture: ProcessArchitecture,
    input: InputIpcListener,
    audio: AudioWorker,
    sequence: AtomicU32,
    started: Instant,
}

impl WindowsGuest {
    fn new(process: TargetProcess, input: InputIpcListener) -> io::Result<Self> {
        let process_id = ProcessId(process.id());
        let audio = match AudioWorker::spawn(process_id) {
            Ok(audio) => audio,
            Err(error) => {
                let _ = process.terminate_tree_and_wait();
                return Err(error);
            }
        };
        Ok(Self {
            architecture: process.architecture(),
            process: Some(process),
            process_id,
            input,
            audio,
            sequence: AtomicU32::new(0),
            started: Instant::now(),
        })
    }

    fn send_hotkey(&self, window: GuestWindowId, chord: HotkeyChord) -> io::Result<()> {
        // Resolve every key before emitting any input, so invalid chords cannot
        // leave modifiers held in the guest.
        let keys = chord
            .modifiers
            .iter()
            .chain(std::iter::once(&chord.key))
            .map(|code| {
                keyboard_code(code).ok_or_else(|| {
                    io::Error::new(
                        io::ErrorKind::InvalidInput,
                        format!("unknown KeyboardEvent code '{code}'"),
                    )
                })
            })
            .collect::<io::Result<Vec<_>>>()?;
        for &key in &keys {
            self.send_key(window, key, true);
        }
        thread::sleep(Duration::from_millis(50));
        for &key in keys.iter().rev() {
            self.send_key(window, key, false);
        }
        Ok(())
    }

    fn send_key(
        &self,
        window: GuestWindowId,
        (virtual_key, scan_code, extended): (u32, u16, bool),
        pressed: bool,
    ) {
        self.input.publish_host_action(HostInputEvent::Key {
            code: KeyCode::new("hotkey"),
            virtual_key: virtual_key as u16,
            scan_code,
            pressed,
            repeat: false,
            extended,
            target_window: window.0,
            timestamp_ms: self.started.elapsed().as_millis() as u32,
            sequence: self.sequence.fetch_add(1, Ordering::AcqRel) + 1,
        });
    }
}

impl Guest for WindowsGuest {
    fn id(&self) -> ProcessId {
        self.process_id
    }
    fn architecture(&self) -> ProcessArchitecture {
        self.architecture
    }

    fn subscribe_exit(&self) -> io::Result<ProcessExitReceiver> {
        let process = self
            .process
            .as_ref()
            .ok_or_else(|| io::Error::new(io::ErrorKind::NotConnected, "guest has exited"))?;
        let wait = process.wait_handle()?;
        let (sender, receiver) = mpsc::channel();
        thread::Builder::new()
            .name("polymer-windows-process-wait".to_owned())
            .spawn(move || {
                let exit = wait
                    .wait()
                    .and_then(|()| wait.exit_code())
                    .map(|code| ProcessExit::Exited { code });
                let _ = sender.send(exit);
            })?;
        Ok(receiver)
    }

    fn window_dimensions(&self, window: GuestWindowId) -> io::Result<ResizeDimensions> {
        client_size(NativeWindow(window.0 as isize))
            .map(|(width, height)| ResizeDimensions::new(width, height))
            .ok_or_else(io::Error::last_os_error)
    }

    fn resize_window(&self, window: GuestWindowId, dimensions: ResizeDimensions) -> io::Result<()> {
        if dimensions.width == 0 || dimensions.height == 0 {
            return Err(io::Error::new(
                io::ErrorKind::InvalidInput,
                "window dimensions must be non-zero",
            ));
        }
        self.input
            .send_control(Message::ResizeWindow(polymer_protocol::ResizeWindow {
                target_window: window.0,
                width: dimensions.width,
                height: dimensions.height,
            }))
            .then_some(())
            .ok_or_else(|| {
                io::Error::new(io::ErrorKind::NotConnected, "guest input is unavailable")
            })
    }

    fn send_input(&self, input: GuestInput) -> io::Result<()> {
        match input {
            GuestInput::Host(event) => {
                self.input.publish_host(event);
                Ok(())
            }
            GuestInput::Hotkey { window, chord } => self.send_hotkey(window, chord),
        }
    }

    fn audio_state(&self) -> io::Result<AudioState> {
        Ok(self.audio.state())
    }
    fn set_volume(&self, volume: NormalizedVolume) -> io::Result<()> {
        self.audio.set_volume(volume);
        Ok(())
    }
    fn set_muted(&self, muted: bool) -> io::Result<()> {
        self.audio.set_muted(muted);
        Ok(())
    }

    fn terminate_tree_and_wait(&mut self) -> io::Result<()> {
        let result = self
            .process
            .take()
            .map_or(Ok(()), TargetProcess::terminate_tree_and_wait);
        self.audio.shutdown();
        result
    }
}

impl Drop for WindowsGuest {
    fn drop(&mut self) {
        let _ = self.terminate_tree_and_wait();
    }
}
