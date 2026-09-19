#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum ProcessArchitecture {
    X86_64,
    Arm64,
}

impl ProcessArchitecture {
    pub const fn executable_suffix(self) -> &'static str {
        match self {
            Self::X86_64 => "x86_64",
            Self::Arm64 => "arm64",
        }
    }
}

#[derive(Clone, Copy, Debug, Default, Eq, PartialEq)]
pub struct ResizeDimensions {
    pub width: u32,
    pub height: u32,
}

impl ResizeDimensions {
    pub const fn new(width: u32, height: u32) -> Self {
        Self { width, height }
    }
}

/// Identifies an OS process without exposing a native process handle.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
#[repr(transparent)]
pub struct ProcessId(pub u32);

/// Guest-local window identity supplied by the capture protocol.
/// Only the platform backend interprets its native representation.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
#[repr(transparent)]
pub struct GuestWindowId(pub i64);

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum ProcessExit {
    Exited { code: u32 },
    Signaled { signal: i32 },
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct HotkeyChord {
    pub modifiers: Vec<String>,
    pub key: String,
}

pub enum GuestInput {
    Host(polymer_input::HostInputEvent),
    Hotkey {
        window: GuestWindowId,
        chord: HotkeyChord,
    },
}

pub type ProcessExitReceiver = std::sync::mpsc::Receiver<std::io::Result<ProcessExit>>;
