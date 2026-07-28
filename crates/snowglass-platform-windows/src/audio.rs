//! Windows Core Audio session control scoped to a process.

use windows::{
    Win32::{
        Media::Audio::{
            IAudioSessionControl2, IAudioSessionManager2, IMMDeviceEnumerator, ISimpleAudioVolume,
            MMDeviceEnumerator, eMultimedia, eRender,
        },
        System::Com::{CLSCTX_ALL, COINIT_MULTITHREADED, CoCreateInstance, CoInitializeEx},
    },
    core::Interface,
};

#[derive(Clone, Copy, Debug, Default, PartialEq)]
pub struct ProcessAudioState {
    pub available: bool,
    pub volume: f32,
    pub muted: bool,
    pub sessions: usize,
}

/// Reads the aggregate audio state for sessions owned by `process_id`.
///
/// When several sessions exist, `volume` is the first session's scalar. The
/// controller always writes every matching session to keep them converged.
pub fn process_audio_state(process_id: u32) -> windows::core::Result<ProcessAudioState> {
    with_sessions(process_id, |sessions| {
        let mut state = ProcessAudioState {
            available: !sessions.is_empty(),
            sessions: sessions.len(),
            ..ProcessAudioState::default()
        };
        if let Some(volume) = sessions.first() {
            state.volume = unsafe { volume.GetMasterVolume()? };
            state.muted = unsafe { volume.GetMute()?.as_bool() };
        }
        Ok(state)
    })
}

/// Applies a scalar volume and mute state to every current session owned by
/// `process_id`, returning how many sessions were updated.
pub fn set_process_audio(
    process_id: u32,
    volume: f32,
    muted: bool,
) -> windows::core::Result<usize> {
    with_sessions(process_id, |sessions| {
        for session in &sessions {
            unsafe {
                session.SetMasterVolume(volume.clamp(0.0, 1.0), std::ptr::null())?;
                session.SetMute(muted, std::ptr::null())?;
            }
        }
        Ok(sessions.len())
    })
}

fn with_sessions<T>(
    process_id: u32,
    use_sessions: impl FnOnce(Vec<ISimpleAudioVolume>) -> windows::core::Result<T>,
) -> windows::core::Result<T> {
    // SAFETY: initializes COM for this caller thread. A thread that was already
    // initialized in a different apartment may return RPC_E_CHANGED_MODE, but
    // its COM apartment is still usable for these agile Core Audio objects.
    let _ = unsafe { CoInitializeEx(None, COINIT_MULTITHREADED) };
    // SAFETY: MMDeviceEnumerator is an in-process COM class with no aggregation.
    let devices: IMMDeviceEnumerator =
        unsafe { CoCreateInstance(&MMDeviceEnumerator, None, CLSCTX_ALL)? };
    let endpoint = unsafe { devices.GetDefaultAudioEndpoint(eRender, eMultimedia)? };
    let manager: IAudioSessionManager2 = unsafe { endpoint.Activate(CLSCTX_ALL, None)? };
    let enumerator = unsafe { manager.GetSessionEnumerator()? };
    let count = unsafe { enumerator.GetCount()? };
    let mut sessions = Vec::new();
    for index in 0..count {
        let control = unsafe { enumerator.GetSession(index)? };
        let control2: IAudioSessionControl2 = control.cast()?;
        if unsafe { control2.GetProcessId()? } != process_id {
            continue;
        }
        sessions.push(control.cast::<ISimpleAudioVolume>()?);
    }
    use_sessions(sessions)
}
