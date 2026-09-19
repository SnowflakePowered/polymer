use crate::{ProcessId, process_audio_state, process_tree_ids, set_process_audio};
use parking_lot::{Condvar, Mutex, RwLock};
use polymer_platform::{AudioState, NormalizedVolume};
use std::{
    sync::Arc,
    thread::{self, JoinHandle},
    time::Duration,
};

struct AudioControl {
    stopping: bool,
    desired: Option<(NormalizedVolume, bool)>,
}

struct AudioShared {
    process_id: ProcessId,
    state: RwLock<AudioState>,
    control: Mutex<AudioControl>,
    wake: Condvar,
}

pub(super) struct AudioWorker {
    shared: Arc<AudioShared>,
    worker: Mutex<Option<JoinHandle<()>>>,
}

impl AudioWorker {
    pub(super) fn spawn(process_id: ProcessId) -> std::io::Result<Self> {
        let shared = Arc::new(AudioShared {
            process_id,
            state: RwLock::new(AudioState::default()),
            control: Mutex::new(AudioControl {
                stopping: false,
                desired: None,
            }),
            wake: Condvar::new(),
        });
        let worker_shared = shared.clone();
        let worker = thread::Builder::new()
            .name(format!("polymer-audio-{}", process_id.0))
            .spawn(move || worker_shared.run())?;
        Ok(Self {
            shared,
            worker: Mutex::new(Some(worker)),
        })
    }

    pub(super) fn state(&self) -> AudioState {
        *self.shared.state.read()
    }

    pub(super) fn set_volume(&self, volume: NormalizedVolume) {
        self.shared.state.write().volume = volume;
        let mut control = self.shared.control.lock();
        let muted = control
            .desired
            .map(|(_, muted)| muted)
            .unwrap_or_else(|| self.shared.state.read().muted);
        control.desired = Some((volume, muted));
        self.shared.wake.notify_one();
    }

    pub(super) fn set_muted(&self, muted: bool) {
        self.shared.state.write().muted = muted;
        let mut control = self.shared.control.lock();
        let volume = control
            .desired
            .map(|(volume, _)| volume)
            .unwrap_or_else(|| self.shared.state.read().volume);
        control.desired = Some((volume, muted));
        self.shared.wake.notify_one();
    }

    pub(super) fn shutdown(&self) {
        {
            let mut control = self.shared.control.lock();
            if !control.stopping {
                control.stopping = true;
                self.shared.wake.notify_one();
            }
        }
        if let Some(worker) = self.worker.lock().take() {
            let _ = worker.join();
        }
    }
}

impl Drop for AudioWorker {
    fn drop(&mut self) {
        self.shutdown();
    }
}

impl AudioShared {
    fn run(self: Arc<Self>) {
        loop {
            self.refresh();
            let mut control = self.control.lock();
            if control.stopping {
                return;
            }
            self.wake.wait_for(&mut control, Duration::from_millis(500));
            if control.stopping {
                return;
            }
        }
    }

    fn refresh(&self) {
        let desired = self.control.lock().desired;
        let processes = process_tree_ids(self.process_id).unwrap_or_else(|_| vec![self.process_id]);
        let mut sessions = 0;

        if let Some((volume, muted)) = desired {
            for process in processes {
                sessions += set_process_audio(process.0, volume.get(), muted).unwrap_or_default();
            }
            *self.state.write() = AudioState {
                available: sessions != 0,
                volume,
                muted,
                active_sessions: sessions,
            };
            return;
        }

        let mut discovered = None;
        for process in processes {
            let Ok(state) = process_audio_state(process.0) else {
                continue;
            };
            sessions += state.sessions;
            if discovered.is_none() && state.available {
                discovered = Some((
                    NormalizedVolume::new(state.volume).unwrap_or_default(),
                    state.muted,
                ));
            }
        }
        let (volume, muted) = discovered.unwrap_or_else(|| {
            let current = *self.state.read();
            (current.volume, current.muted)
        });
        *self.state.write() = AudioState {
            available: sessions != 0,
            volume,
            muted,
            active_sessions: sessions,
        };
    }
}
