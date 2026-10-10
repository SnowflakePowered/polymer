use crate::hooks::{
    HookInstallation, XinputGetStateContext, XinputGetStateExContext, XinputSetStateContext,
};
use parking_lot::Mutex;
use polymer_hook::config;
use polymer_hook_input::HookInput;
use polymer_input::{Feedback, GamepadButton, GamepadReport};
use std::sync::{
    Arc,
    atomic::{AtomicBool, Ordering},
};
use windows_sys::{
    Win32::{
        Foundation::{ERROR_BAD_ARGUMENTS, ERROR_DEVICE_NOT_CONNECTED, ERROR_SUCCESS, HMODULE},
        System::LibraryLoader::{GetProcAddress, LoadLibraryW},
        UI::Input::XboxController::{
            XINPUT_GAMEPAD, XINPUT_GAMEPAD_A, XINPUT_GAMEPAD_B, XINPUT_GAMEPAD_BACK,
            XINPUT_GAMEPAD_DPAD_DOWN, XINPUT_GAMEPAD_DPAD_LEFT, XINPUT_GAMEPAD_DPAD_RIGHT,
            XINPUT_GAMEPAD_DPAD_UP, XINPUT_GAMEPAD_LEFT_SHOULDER, XINPUT_GAMEPAD_LEFT_THUMB,
            XINPUT_GAMEPAD_RIGHT_SHOULDER, XINPUT_GAMEPAD_RIGHT_THUMB, XINPUT_GAMEPAD_START,
            XINPUT_GAMEPAD_X, XINPUT_GAMEPAD_Y, XINPUT_STATE, XINPUT_VIBRATION,
        },
    },
    core::PCSTR,
};

pub(crate) type GetStateFn = unsafe extern "system" fn(u32, *mut XINPUT_STATE) -> u32;
pub(crate) type SetStateFn = unsafe extern "system" fn(u32, *const XINPUT_VIBRATION) -> u32;

const USER_COUNT: usize = 4;
const GUIDE: u16 = 0x0400;
const GET_STATE_EX: PCSTR = 100 as PCSTR;
const BUTTONS: [(GamepadButton, u16); 15] = [
    (GamepadButton::A, XINPUT_GAMEPAD_A),
    (GamepadButton::B, XINPUT_GAMEPAD_B),
    (GamepadButton::X, XINPUT_GAMEPAD_X),
    (GamepadButton::Y, XINPUT_GAMEPAD_Y),
    (GamepadButton::Select, XINPUT_GAMEPAD_BACK),
    (GamepadButton::Guide, GUIDE),
    (GamepadButton::Start, XINPUT_GAMEPAD_START),
    (GamepadButton::LeftStick, XINPUT_GAMEPAD_LEFT_THUMB),
    (GamepadButton::RightStick, XINPUT_GAMEPAD_RIGHT_THUMB),
    (GamepadButton::L, XINPUT_GAMEPAD_LEFT_SHOULDER),
    (GamepadButton::R, XINPUT_GAMEPAD_RIGHT_SHOULDER),
    (GamepadButton::DpadUp, XINPUT_GAMEPAD_DPAD_UP),
    (GamepadButton::DpadDown, XINPUT_GAMEPAD_DPAD_DOWN),
    (GamepadButton::DpadLeft, XINPUT_GAMEPAD_DPAD_LEFT),
    (GamepadButton::DpadRight, XINPUT_GAMEPAD_DPAD_RIGHT),
];

pub(crate) struct Runtime {
    pub(crate) get_state: GetStateFn,
    pub(crate) get_state_ex: GetStateFn,
    pub(crate) set_state: SetStateFn,
    input: Option<Arc<HookInput>>,
    packets: Mutex<[(GamepadReport, u32); USER_COUNT]>,
    logged_poll: AtomicBool,
    logged_connection: AtomicBool,
}

impl Default for Runtime {
    fn default() -> Self {
        polymer_hook::telemetry::init_file_tracing(config::log_file(config::XINPUT_LOG_FILE));
        let module = unsafe { LoadLibraryW(windows_sys::w!("xinput1_4.dll")) };
        assert!(!module.is_null(), "xinput1_4.dll is unavailable");
        let session = config::session().ok();
        Self {
            get_state: unsafe { Self::export(module, c"XInputGetState".as_ptr().cast()) },
            get_state_ex: unsafe { Self::export(module, GET_STATE_EX) },
            set_state: unsafe { Self::export(module, c"XInputSetState".as_ptr().cast()) },
            input: session.as_deref().map(HookInput::start),
            packets: Mutex::new([(GamepadReport::default(), 0); USER_COUNT]),
            logged_poll: AtomicBool::new(false),
            logged_connection: AtomicBool::new(false),
        }
    }
}

impl Runtime {
    pub(crate) fn target<T>(select: impl FnOnce(&Self) -> T) -> T {
        HookInstallation::with_state(select).expect("XInput hook state is installed")
    }

    unsafe fn export<T: Copy>(module: HMODULE, name: PCSTR) -> T {
        let address = unsafe { GetProcAddress(module, name) }
            .expect("xinput1_4.dll exports every hooked entry point");
        unsafe { std::mem::transmute_copy(&address) }
    }

    pub(crate) unsafe fn get_state(
        user: u32,
        state: *mut XINPUT_STATE,
        next: XinputGetStateContext<'_>,
    ) -> u32 {
        unsafe { next.state.fill(user, state, !GUIDE) }
    }

    pub(crate) unsafe fn get_state_ex(
        user: u32,
        state: *mut XINPUT_STATE,
        next: XinputGetStateExContext<'_>,
    ) -> u32 {
        unsafe { next.state.fill(user, state, u16::MAX) }
    }

    pub(crate) unsafe fn set_state(
        user: u32,
        vibration: *const XINPUT_VIBRATION,
        next: XinputSetStateContext<'_>,
    ) -> u32 {
        let Some((input, player)) = next.state.connected(user) else {
            return ERROR_DEVICE_NOT_CONNECTED;
        };
        if let Some(vibration) = unsafe { vibration.as_ref() } {
            input.send_feedback(
                player,
                Feedback::Rumble {
                    low_frequency: vibration.wLeftMotorSpeed,
                    high_frequency: vibration.wRightMotorSpeed,
                    duration_ms: 0,
                },
            );
        }
        ERROR_SUCCESS
    }

    fn connected(&self, user: u32) -> Option<(&HookInput, u8)> {
        let input = self.input.as_deref()?;
        let player = u8::try_from(user)
            .ok()
            .filter(|player| usize::from(*player) < USER_COUNT.min(input.topology().pad_count()))?;
        Some((input, player))
    }

    unsafe fn fill(&self, user: u32, state: *mut XINPUT_STATE, mask: u16) -> u32 {
        if !self.logged_poll.swap(true, Ordering::AcqRel) {
            tracing::debug!("target first polled XInput");
        }
        let Some((input, player)) = self.connected(user) else {
            return ERROR_DEVICE_NOT_CONNECTED;
        };
        let Some(state) = (unsafe { state.as_mut() }) else {
            return ERROR_BAD_ARGUMENTS;
        };
        if !self.logged_connection.swap(true, Ordering::AcqRel) {
            tracing::debug!("reported a synthetic XInput pad on user {user}");
        }
        let report = input
            .mapped(player)
            .or_else(|| input.raw(player).map(|raw| raw.gamepad))
            .unwrap_or_default();
        let mut packets = self.packets.lock();
        let (published, packet) = &mut packets[usize::from(player)];
        if *published != report {
            *published = report;
            *packet = packet.wrapping_add(1);
        }
        *state = XINPUT_STATE {
            dwPacketNumber: *packet,
            Gamepad: XINPUT_GAMEPAD {
                wButtons: BUTTONS
                    .iter()
                    .filter(|(button, _)| report.buttons & button.mask() != 0)
                    .fold(0, |buttons, (_, flag)| buttons | flag)
                    & mask,
                bLeftTrigger: (report.left_trigger >> 8) as u8,
                bRightTrigger: (report.right_trigger >> 8) as u8,
                sThumbLX: report.left_stick[0],
                sThumbLY: report.left_stick[1].saturating_neg(),
                sThumbRX: report.right_stick[0],
                sThumbRY: report.right_stick[1].saturating_neg(),
            },
        };
        ERROR_SUCCESS
    }
}
