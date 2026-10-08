use std::{path::PathBuf, time::Instant};
use winit::{
    event::{DeviceEvent, ElementState, Ime, MouseButton, MouseScrollDelta, WindowEvent},
    keyboard::{Key, NamedKey, PhysicalKey},
};
#[derive(Debug)]
pub enum WindowCommand {
    WakeCapture,
    CompositorTick,
    ShellRasterReady,
    ShellUpdated,
    SetCursor(Option<cursor_icon::CursorIcon>),
    SetTitle(String),
    SetImeEnabled(bool),
    SetImeCursorArea {
        x: f32,
        y: f32,
        width: f32,
        height: f32,
    },
}

pub enum LoopDirective {
    Exit,
    WaitUntil(Instant),
}

#[derive(Clone, Debug)]
pub enum ShellWindowEvent {
    CloseRequested,
    DroppedFile(PathBuf),
    FileHover(bool),
    Moved,
    Resized {
        width: u32,
        height: u32,
    },
    ScaleFactorChanged,
    FocusLost,
    Keyboard {
        physical_key: Option<String>,
        logical_key: String,
        pressed: bool,
        repeat: bool,
    },
    Modifiers {
        alt: bool,
        shift: bool,
        control: bool,
    },
    CursorMoved {
        x: f64,
        y: f64,
    },
    CursorLeft,
    MouseInput {
        button: Option<u16>,
        pressed: bool,
    },
    MouseWheel {
        horizontal: i32,
        vertical: i32,
    },
    Ime(ShellImeEvent),
    RedrawRequested,
    Other,
}

#[derive(Clone, Debug)]
pub enum ShellImeEvent {
    Enabled,
    Preedit(String, Option<(usize, usize)>),
    Commit(String),
    Disabled,
}

#[derive(Clone, Copy, Debug)]
pub enum ShellDeviceEvent {
    MouseMotion { delta_x: f64, delta_y: f64 },
    Other,
}

pub fn translate_device_event(event: DeviceEvent) -> ShellDeviceEvent {
    match event {
        DeviceEvent::MouseMotion { delta } => ShellDeviceEvent::MouseMotion {
            delta_x: delta.0,
            delta_y: delta.1,
        },
        _ => ShellDeviceEvent::Other,
    }
}

pub fn translate_window_event(event: WindowEvent) -> ShellWindowEvent {
    match event {
        WindowEvent::CloseRequested | WindowEvent::Destroyed => ShellWindowEvent::CloseRequested,
        WindowEvent::DroppedFile(path) => ShellWindowEvent::DroppedFile(path),
        WindowEvent::HoveredFile(_) => ShellWindowEvent::FileHover(true),
        WindowEvent::HoveredFileCancelled => ShellWindowEvent::FileHover(false),
        WindowEvent::Moved(_) => ShellWindowEvent::Moved,
        WindowEvent::Resized(size) => ShellWindowEvent::Resized {
            width: size.width,
            height: size.height,
        },
        WindowEvent::ScaleFactorChanged { .. } => ShellWindowEvent::ScaleFactorChanged,
        WindowEvent::Focused(false) => ShellWindowEvent::FocusLost,
        WindowEvent::Focused(true) => ShellWindowEvent::Other,
        WindowEvent::KeyboardInput { event, .. } => {
            let physical_key = match event.physical_key {
                PhysicalKey::Code(code) => Some(format!("{code:?}")),
                PhysicalKey::Unidentified(_) => None,
            };
            let logical_key = match event.logical_key {
                Key::Named(NamedKey::Space) => " ".into(),
                Key::Named(key) => format!("{key:?}"),
                Key::Character(key) => key.to_string(),
                Key::Dead(_) => "Dead".into(),
                Key::Unidentified(_) => "Unidentified".into(),
            };
            ShellWindowEvent::Keyboard {
                physical_key,
                logical_key,
                pressed: event.state == ElementState::Pressed,
                repeat: event.repeat,
            }
        }
        WindowEvent::ModifiersChanged(modifiers) => ShellWindowEvent::Modifiers {
            alt: modifiers.state().alt_key(),
            shift: modifiers.state().shift_key(),
            control: modifiers.state().control_key(),
        },
        WindowEvent::CursorMoved { position, .. } => ShellWindowEvent::CursorMoved {
            x: position.x,
            y: position.y,
        },
        WindowEvent::CursorLeft { .. } => ShellWindowEvent::CursorLeft,
        WindowEvent::MouseInput { state, button, .. } => ShellWindowEvent::MouseInput {
            button: neutral_mouse_button(button),
            pressed: state == ElementState::Pressed,
        },
        WindowEvent::MouseWheel { delta, .. } => {
            let (horizontal, vertical) = match delta {
                MouseScrollDelta::LineDelta(horizontal, vertical) => {
                    ((horizontal * 120.0) as i32, (vertical * 120.0) as i32)
                }
                MouseScrollDelta::PixelDelta(delta) => (delta.x as i32, delta.y as i32),
            };
            ShellWindowEvent::MouseWheel {
                horizontal,
                vertical,
            }
        }
        WindowEvent::Ime(event) => ShellWindowEvent::Ime(match event {
            Ime::Enabled => ShellImeEvent::Enabled,
            Ime::Preedit(value, range) => ShellImeEvent::Preedit(value, range),
            Ime::Commit(value) => ShellImeEvent::Commit(value),
            Ime::Disabled => ShellImeEvent::Disabled,
        }),
        WindowEvent::RedrawRequested => ShellWindowEvent::RedrawRequested,
        _ => ShellWindowEvent::Other,
    }
}

fn neutral_mouse_button(button: MouseButton) -> Option<u16> {
    match button {
        MouseButton::Left => Some(1),
        MouseButton::Right => Some(2),
        MouseButton::Middle => Some(3),
        MouseButton::Back => Some(4),
        MouseButton::Forward => Some(5),
        MouseButton::Other(button) => Some(button.saturating_add(4)),
    }
}
