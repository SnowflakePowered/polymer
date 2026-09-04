use polymer_automation::{AutomationError, InputCommand};
use windows::Win32::Foundation::{HWND, LPARAM, POINT, WPARAM};
use windows::Win32::Graphics::Gdi::ScreenToClient;
use windows::Win32::UI::WindowsAndMessaging::{
    PostMessageW, WA_ACTIVE, WM_ACTIVATE, WM_ACTIVATEAPP, WM_CHAR, WM_KEYDOWN, WM_KEYUP,
    WM_LBUTTONDOWN, WM_LBUTTONUP, WM_MBUTTONDOWN, WM_MBUTTONUP, WM_MOUSEMOVE, WM_MOUSEWHEEL,
    WM_RBUTTONDOWN, WM_RBUTTONUP, WM_SETFOCUS, WM_XBUTTONDOWN, WM_XBUTTONUP,
};

pub(super) struct TargetedWindowInput(HWND);

impl TargetedWindowInput {
    pub fn new(native: isize) -> Self {
        Self(HWND(native as *mut _))
    }

    pub fn execute(&self, command: InputCommand) -> Result<(), AutomationError> {
        match command {
            InputCommand::LogicalFocus => self.logical_focus(),
            InputCommand::KeyDown { code } => self.key(&code, true, false),
            InputCommand::KeyUp { code } => self.key(&code, false, false),
            InputCommand::KeyPress {
                code,
                modifiers,
                repeat,
            } => {
                let key = Self::virtual_key(&code)?;
                let modifiers = modifiers
                    .iter()
                    .map(|modifier| Self::virtual_key(modifier))
                    .collect::<Result<Vec<_>, _>>()?;
                self.logical_focus()?;
                for modifier in &modifiers {
                    self.key_code(*modifier, true, false)?;
                }
                self.key_code(key, true, repeat)?;
                self.key_code(key, false, repeat)?;
                for modifier in modifiers.iter().rev() {
                    self.key_code(*modifier, false, false)?;
                }
                Ok(())
            }
            InputCommand::TypeText { text } => {
                for unit in text.encode_utf16() {
                    self.post(WM_CHAR, unit as usize, 1)?;
                }
                Ok(())
            }
            InputCommand::MouseMove { x, y } => self.mouse(WM_MOUSEMOVE, 0, x, y),
            InputCommand::MouseDown { button, x, y } => {
                let (down, _, down_data, _) = Self::button(&button)?;
                self.mouse(down, down_data, x, y)
            }
            InputCommand::MouseUp { button, x, y } => {
                let (_, up, _, up_data) = Self::button(&button)?;
                self.mouse(up, up_data, x, y)
            }
            InputCommand::MouseClick {
                button,
                count,
                x,
                y,
            } => {
                if count == 0 || count > 3 {
                    return Err(AutomationError::invalid_argument(
                        "mouse click count must be between 1 and 3",
                    ));
                }
                let (down, up, down_data, up_data) = Self::button(&button)?;
                for _ in 0..count {
                    self.mouse(down, down_data, x, y)?;
                    self.mouse(up, up_data, x, y)?;
                }
                Ok(())
            }
            InputCommand::MouseWheel { delta_x, delta_y } => {
                if delta_x != 0 {
                    self.post(
                        windows::Win32::UI::WindowsAndMessaging::WM_MOUSEHWHEEL,
                        (delta_x as i16 as u16 as usize) << 16,
                        0,
                    )?;
                }
                if delta_y != 0 {
                    self.post(WM_MOUSEWHEEL, (delta_y as i16 as u16 as usize) << 16, 0)?;
                }
                Ok(())
            }
        }
    }

    fn logical_focus(&self) -> Result<(), AutomationError> {
        self.post(WM_ACTIVATEAPP, 1, 0)?;
        self.post(WM_ACTIVATE, WA_ACTIVE as usize, 0)?;
        self.post(WM_SETFOCUS, 0, 0)
    }

    fn key(&self, code: &str, pressed: bool, repeat: bool) -> Result<(), AutomationError> {
        let key = Self::virtual_key(code)?;
        if pressed {
            self.logical_focus()?;
        }
        self.key_code(key, pressed, repeat)
    }

    fn key_code(&self, key: u32, pressed: bool, repeat: bool) -> Result<(), AutomationError> {
        use windows::Win32::UI::Input::KeyboardAndMouse::{MAPVK_VK_TO_VSC_EX, MapVirtualKeyW};
        let mapped = unsafe { MapVirtualKeyW(key, MAPVK_VK_TO_VSC_EX) };
        let mut parameters = 1isize | (((mapped & 0xff) as isize) << 16);
        if matches!((mapped >> 8) & 0xff, 0xe0 | 0xe1) {
            parameters |= 1 << 24;
        }
        if repeat || !pressed {
            parameters |= 1 << 30;
        }
        if !pressed {
            parameters |= 1 << 31;
        }
        self.post(
            if pressed { WM_KEYDOWN } else { WM_KEYUP },
            key as usize,
            parameters,
        )
    }

    fn mouse(&self, message: u32, data: usize, x: i32, y: i32) -> Result<(), AutomationError> {
        let mut point = POINT { x, y };
        unsafe { ScreenToClient(self.0, &mut point) }
            .ok()
            .map_err(|error| AutomationError::platform(error.to_string()))?;
        let coordinates = ((point.y as u16 as u32) << 16 | point.x as u16 as u32) as isize;
        self.post(message, data, coordinates)
    }

    fn post(&self, message: u32, wparam: usize, lparam: isize) -> Result<(), AutomationError> {
        unsafe { PostMessageW(Some(self.0), message, WPARAM(wparam), LPARAM(lparam)) }
            .map_err(|error| AutomationError::platform(error.to_string()))
    }

    fn button(button: &str) -> Result<(u32, u32, usize, usize), AutomationError> {
        use windows::Win32::System::SystemServices::{
            MK_LBUTTON, MK_MBUTTON, MK_RBUTTON, MK_XBUTTON1, MK_XBUTTON2,
        };
        match button {
            "left" => Ok((WM_LBUTTONDOWN, WM_LBUTTONUP, MK_LBUTTON.0 as usize, 0)),
            "right" => Ok((WM_RBUTTONDOWN, WM_RBUTTONUP, MK_RBUTTON.0 as usize, 0)),
            "middle" => Ok((WM_MBUTTONDOWN, WM_MBUTTONUP, MK_MBUTTON.0 as usize, 0)),
            "x1" => Ok((
                WM_XBUTTONDOWN,
                WM_XBUTTONUP,
                MK_XBUTTON1.0 as usize | 1 << 16,
                1 << 16,
            )),
            "x2" => Ok((
                WM_XBUTTONDOWN,
                WM_XBUTTONUP,
                MK_XBUTTON2.0 as usize | 2 << 16,
                2 << 16,
            )),
            _ => Err(AutomationError::invalid_argument(format!(
                "unknown mouse button '{button}'"
            ))),
        }
    }

    fn virtual_key(code: &str) -> Result<u32, AutomationError> {
        crate::window::keyboard_code(code)
            .map(|(virtual_key, _, _)| virtual_key)
            .ok_or_else(|| AutomationError::invalid_argument(format!("unknown key '{code}'")))
    }
}
