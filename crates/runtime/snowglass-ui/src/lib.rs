//! Guest modal-UI forwarding policy shared by hooks and the viewer.

use snowglass_ipc::{
    LocalSocket,
    ui::{self, DialogButton, MessageBoxRequest, MessageBoxResponse},
};

pub const IDOK: i32 = 1;
pub const IDCANCEL: i32 = 2;
pub const IDABORT: i32 = 3;
pub const IDRETRY: i32 = 4;
pub const IDIGNORE: i32 = 5;
pub const IDYES: i32 = 6;
pub const IDNO: i32 = 7;
pub const IDTRYAGAIN: i32 = 10;
pub const IDCONTINUE: i32 = 11;

const MB_TYPEMASK: u32 = 0xf;
const MB_OK: u32 = 0;
const MB_OKCANCEL: u32 = 1;
const MB_ABORTRETRYIGNORE: u32 = 2;
const MB_YESNOCANCEL: u32 = 3;
const MB_YESNO: u32 = 4;
const MB_RETRYCANCEL: u32 = 5;
const MB_CANCELTRYCONTINUE: u32 = 6;
const MB_DEFBUTTON2: u32 = 0x100;
const MB_DEFBUTTON3: u32 = 0x200;
const MB_DEFBUTTON4: u32 = 0x300;

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct MessageBoxButton {
    pub result: i32,
    pub label: &'static str,
}

pub fn message_box_buttons(style: u32) -> &'static [MessageBoxButton] {
    const OK: &[MessageBoxButton] = &[MessageBoxButton {
        result: IDOK,
        label: "OK",
    }];
    const OK_CANCEL: &[MessageBoxButton] = &[
        MessageBoxButton {
            result: IDOK,
            label: "OK",
        },
        MessageBoxButton {
            result: IDCANCEL,
            label: "Cancel",
        },
    ];
    const ABORT_RETRY_IGNORE: &[MessageBoxButton] = &[
        MessageBoxButton {
            result: IDABORT,
            label: "Abort",
        },
        MessageBoxButton {
            result: IDRETRY,
            label: "Retry",
        },
        MessageBoxButton {
            result: IDIGNORE,
            label: "Ignore",
        },
    ];
    const YES_NO_CANCEL: &[MessageBoxButton] = &[
        MessageBoxButton {
            result: IDYES,
            label: "Yes",
        },
        MessageBoxButton {
            result: IDNO,
            label: "No",
        },
        MessageBoxButton {
            result: IDCANCEL,
            label: "Cancel",
        },
    ];
    const YES_NO: &[MessageBoxButton] = &[
        MessageBoxButton {
            result: IDYES,
            label: "Yes",
        },
        MessageBoxButton {
            result: IDNO,
            label: "No",
        },
    ];
    const RETRY_CANCEL: &[MessageBoxButton] = &[
        MessageBoxButton {
            result: IDRETRY,
            label: "Retry",
        },
        MessageBoxButton {
            result: IDCANCEL,
            label: "Cancel",
        },
    ];
    const CANCEL_TRY_CONTINUE: &[MessageBoxButton] = &[
        MessageBoxButton {
            result: IDCANCEL,
            label: "Cancel",
        },
        MessageBoxButton {
            result: IDTRYAGAIN,
            label: "Try Again",
        },
        MessageBoxButton {
            result: IDCONTINUE,
            label: "Continue",
        },
    ];
    match style & MB_TYPEMASK {
        MB_OK => OK,
        MB_OKCANCEL => OK_CANCEL,
        MB_ABORTRETRYIGNORE => ABORT_RETRY_IGNORE,
        MB_YESNOCANCEL => YES_NO_CANCEL,
        MB_YESNO => YES_NO,
        MB_RETRYCANCEL => RETRY_CANCEL,
        MB_CANCELTRYCONTINUE => CANCEL_TRY_CONTINUE,
        _ => OK,
    }
}

pub fn default_button(style: u32) -> usize {
    match style & 0x300 {
        MB_DEFBUTTON2 => 1,
        MB_DEFBUTTON3 => 2,
        MB_DEFBUTTON4 => 3,
        _ => 0,
    }
}

pub fn escape_button(style: u32) -> Option<i32> {
    match style & MB_TYPEMASK {
        MB_OK => Some(IDOK),
        MB_OKCANCEL | MB_YESNOCANCEL | MB_RETRYCANCEL | MB_CANCELTRYCONTINUE => Some(IDCANCEL),
        _ => None,
    }
}

pub fn native_buttons(style: u32) -> Vec<DialogButton> {
    message_box_buttons(style)
        .iter()
        .map(|button| DialogButton {
            result: button.result,
            label: button.label.into(),
        })
        .collect()
}

pub fn native_default_result(style: u32) -> Option<i32> {
    let buttons = message_box_buttons(style);
    buttons
        .get(default_button(style))
        .or_else(|| buttons.first())
        .map(|button| button.result)
}

/// Returns `None` when forwarding is unavailable, allowing the hook to invoke
/// the original API instead of swallowing an application dialog.
pub fn forward_message_box(session: &str, request: &MessageBoxRequest) -> Option<i32> {
    if request.protocol_version != ui::UI_PROTOCOL_VERSION {
        return None;
    }
    let mut socket = LocalSocket::connect(&format!("{session}-ui")).ok()?;
    ui::send(&mut socket, request).ok()?;
    let response: MessageBoxResponse = ui::receive(&mut socket).ok()?;
    request
        .buttons
        .iter()
        .any(|button| button.result == response.result)
        .then_some(response.result)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn decodes_button_sets_and_defaults() {
        assert_eq!(
            message_box_buttons(MB_YESNOCANCEL)
                .iter()
                .map(|button| button.result)
                .collect::<Vec<_>>(),
            [IDYES, IDNO, IDCANCEL]
        );
        assert_eq!(default_button(MB_YESNOCANCEL | MB_DEFBUTTON2), 1);
        assert_eq!(escape_button(MB_YESNO), None);
        assert_eq!(escape_button(MB_OKCANCEL), Some(IDCANCEL));
    }

    #[test]
    fn forwarding_returns_the_viewer_selection() {
        let session = format!(
            "snowglass-ui-forward-test-{}-{}",
            std::process::id(),
            std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .unwrap()
                .as_nanos()
        );
        let listener = snowglass_ipc::LocalSocketListener::bind(&format!("{session}-ui")).unwrap();
        let server = std::thread::spawn(move || {
            let mut socket = listener.accept().unwrap();
            let request: MessageBoxRequest = ui::receive(&mut socket).unwrap();
            assert_eq!(request.caption, "Forwarded");
            ui::send(&mut socket, &MessageBoxResponse { result: IDNO }).unwrap();
        });
        let result = forward_message_box(
            &session,
            &MessageBoxRequest {
                protocol_version: ui::UI_PROTOCOL_VERSION,
                process_id: 1,
                thread_id: 2,
                owner_window: 0,
                caption: "Forwarded".into(),
                text: "Continue?".into(),
                style: MB_YESNO,
                buttons: native_buttons(MB_YESNO),
                default_result: native_default_result(MB_YESNO),
                escape_result: escape_button(MB_YESNO),
            },
        );
        assert_eq!(result, Some(IDNO));
        server.join().unwrap();
    }
}
