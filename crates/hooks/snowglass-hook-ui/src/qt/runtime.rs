//! Runtime-resolved Qt 6 `QMessageBox` interception.

use snowglass_ipc::ui::{DialogButton, MessageBoxRequest, UI_PROTOCOL_VERSION};
use std::{
    ffi::{CStr, c_void},
    mem::MaybeUninit,
    sync::OnceLock,
};
use windows_sys::Win32::{
    Foundation::HMODULE,
    System::{
        LibraryLoader::{GetModuleHandleW, GetProcAddress},
        Threading::{GetCurrentProcessId, GetCurrentThreadId},
    },
};

type DialogExecFn = unsafe extern "C" fn(*mut c_void) -> i32;
type InheritsFn = unsafe extern "C" fn(*const c_void, *const u8) -> u8;
type QStringDestructorFn = unsafe extern "C" fn(*mut QtString);
type StringGetterFn = unsafe extern "C" fn(*const c_void, *mut QtString) -> *mut QtString;
type FlagsGetterFn = unsafe extern "C" fn(*const c_void, *mut i32) -> *mut i32;
type IntegerGetterFn = unsafe extern "C" fn(*const c_void) -> i32;
type PointerGetterFn = unsafe extern "C" fn(*const c_void) -> *mut c_void;
type StandardButtonFn = unsafe extern "C" fn(*const c_void, *mut c_void) -> i32;
type ButtonFn = unsafe extern "C" fn(*mut c_void, i32) -> *mut c_void;
type ClickFn = unsafe extern "C" fn(*mut c_void);

#[repr(C)]
struct QtString {
    // QString contains a QArrayDataPointer<char16_t>.
    allocation: *mut c_void,
    data: *const u16,
    size: isize,
}

struct QtApi {
    dialog_exec: DialogExecFn,
    inherits: InheritsFn,
    string_destructor: QStringDestructorFn,
    window_title: StringGetterFn,
    text: StringGetterFn,
    informative_text: StringGetterFn,
    detailed_text: StringGetterFn,
    standard_buttons: FlagsGetterFn,
    icon: IntegerGetterFn,
    default_button: PointerGetterFn,
    escape_button: PointerGetterFn,
    standard_button: StandardButtonFn,
    button: ButtonFn,
    click: ClickFn,
}

static API: OnceLock<QtApi> = OnceLock::new();

const QT6_WIDGETS: &[u16] = &[
    b'Q' as u16,
    b't' as u16,
    b'6' as u16,
    b'W' as u16,
    b'i' as u16,
    b'd' as u16,
    b'g' as u16,
    b'e' as u16,
    b't' as u16,
    b's' as u16,
    b'.' as u16,
    b'd' as u16,
    b'l' as u16,
    b'l' as u16,
    0,
];
const QT6_CORE: &[u16] = &[
    b'Q' as u16,
    b't' as u16,
    b'6' as u16,
    b'C' as u16,
    b'o' as u16,
    b'r' as u16,
    b'e' as u16,
    b'.' as u16,
    b'd' as u16,
    b'l' as u16,
    b'l' as u16,
    0,
];

pub(crate) fn install() {
    let widgets = unsafe { GetModuleHandleW(QT6_WIDGETS.as_ptr()) };
    let core = unsafe { GetModuleHandleW(QT6_CORE.as_ptr()) };
    if widgets.is_null() || core.is_null() {
        return;
    }
    let Some(api) = (unsafe { QtApi::load(widgets, core) }) else {
        return;
    };
    if API.set(api).is_ok() {
        let _ = super::hooks::QtHookInstallation::install();
    }
}

impl QtApi {
    unsafe fn load(widgets: HMODULE, core: HMODULE) -> Option<Self> {
        Some(Self {
            dialog_exec: unsafe { Self::symbol(widgets, c"?exec@QDialog@@UEAAHXZ") }?,
            inherits: unsafe { Self::symbol(core, c"?inherits@QObject@@QEBA_NPEBD@Z") }?,
            string_destructor: unsafe { Self::symbol(core, c"??1QString@@QEAA@XZ") }?,
            window_title: unsafe {
                Self::symbol(widgets, c"?windowTitle@QWidget@@QEBA?AVQString@@XZ")
            }?,
            text: unsafe { Self::symbol(widgets, c"?text@QMessageBox@@QEBA?AVQString@@XZ") }?,
            informative_text: unsafe {
                Self::symbol(widgets, c"?informativeText@QMessageBox@@QEBA?AVQString@@XZ")
            }?,
            detailed_text: unsafe {
                Self::symbol(widgets, c"?detailedText@QMessageBox@@QEBA?AVQString@@XZ")
            }?,
            standard_buttons: unsafe {
                Self::symbol(
                    widgets,
                    c"?standardButtons@QMessageBox@@QEBA?AV?$QFlags@W4StandardButton@QMessageBox@@@@XZ",
                )
            }?,
            icon: unsafe { Self::symbol(widgets, c"?icon@QMessageBox@@QEBA?AW4Icon@1@XZ") }?,
            default_button: unsafe {
                Self::symbol(
                    widgets,
                    c"?defaultButton@QMessageBox@@QEBAPEAVQPushButton@@XZ",
                )
            }?,
            escape_button: unsafe {
                Self::symbol(
                    widgets,
                    c"?escapeButton@QMessageBox@@QEBAPEAVQAbstractButton@@XZ",
                )
            }?,
            standard_button: unsafe {
                Self::symbol(
                    widgets,
                    c"?standardButton@QMessageBox@@QEBA?AW4StandardButton@1@PEAVQAbstractButton@@@Z",
                )
            }?,
            button: unsafe {
                Self::symbol(
                    widgets,
                    c"?button@QMessageBox@@QEBAPEAVQAbstractButton@@W4StandardButton@1@@Z",
                )
            }?,
            click: unsafe { Self::symbol(widgets, c"?click@QAbstractButton@@QEAAXXZ") }?,
        })
    }

    unsafe fn symbol<T: Copy>(module: HMODULE, name: &CStr) -> Option<T> {
        let address = unsafe { GetProcAddress(module, name.as_ptr().cast()) }?;
        // SAFETY: each caller supplies the ABI matching the named Qt export.
        Some(unsafe { std::mem::transmute_copy(&address) })
    }
}

pub(super) fn dialog_exec_target() -> DialogExecFn {
    API.get().expect("Qt API was loaded").dialog_exec
}

pub(super) unsafe fn dialog_exec(
    dialog: *mut c_void,
    mut context: super::hooks::QtDialogExecContext<'_>,
) -> i32 {
    let Some(api) = API.get() else {
        return snowglass_hook::next!(context, dialog);
    };
    if unsafe { (api.inherits)(dialog, c"QMessageBox".as_ptr().cast()) } == 0 {
        return snowglass_hook::next!(context, dialog);
    }
    let Some(result) = (unsafe { api.forward(dialog) }) else {
        return snowglass_hook::next!(context, dialog);
    };
    let button = unsafe { (api.button)(dialog, result) };
    if !button.is_null() {
        // This preserves QMessageBox::clickedButton(), result(), and buttonClicked.
        unsafe { (api.click)(button) };
    }
    result
}

impl QtApi {
    unsafe fn forward(&self, dialog: *mut c_void) -> Option<i32> {
        let button_mask = unsafe { Self::get_flags(self.standard_buttons, dialog) };
        let buttons = Self::buttons(button_mask)?;
        let default_button = unsafe { self.standard_button(dialog, (self.default_button)(dialog)) };
        let explicit_escape = unsafe { self.standard_button(dialog, (self.escape_button)(dialog)) };
        let primary = unsafe { self.get_string(self.text, dialog) };
        let informative = unsafe { self.get_string(self.informative_text, dialog) };
        let details = unsafe { self.get_string(self.detailed_text, dialog) };
        let text = [primary, informative]
            .into_iter()
            .filter(|value| !value.is_empty())
            .collect::<Vec<_>>()
            .join("\n\n");
        let text = if details.is_empty() {
            text
        } else {
            format!("{text}\n\nDetails:\n{details}")
        };
        let request = MessageBoxRequest {
            protocol_version: UI_PROTOCOL_VERSION,
            process_id: unsafe { GetCurrentProcessId() },
            thread_id: unsafe { GetCurrentThreadId() },
            owner_window: 0,
            caption: unsafe { self.get_string(self.window_title, dialog) },
            text,
            style: Self::icon_style(unsafe { (self.icon)(dialog) }),
            default_result: default_button.or_else(|| buttons.first().map(|button| button.result)),
            escape_result: explicit_escape.or_else(|| Self::escape(&buttons)),
            buttons,
        };
        let session = snowglass_hook::config::session().ok()?;
        snowglass_ui::forward_message_box(&session, &request)
    }

    unsafe fn get_flags(getter: FlagsGetterFn, object: *mut c_void) -> i32 {
        let mut value = MaybeUninit::<i32>::uninit();
        unsafe { getter(object, value.as_mut_ptr()) };
        unsafe { value.assume_init() }
    }

    unsafe fn standard_button(&self, dialog: *mut c_void, button: *mut c_void) -> Option<i32> {
        if button.is_null() {
            None
        } else {
            let result = unsafe { (self.standard_button)(dialog, button) };
            (result != 0).then_some(result)
        }
    }

    unsafe fn get_string(&self, getter: StringGetterFn, object: *mut c_void) -> String {
        let mut value = MaybeUninit::<QtString>::uninit();
        unsafe { getter(object, value.as_mut_ptr()) };
        let mut value = unsafe { value.assume_init() };
        let result = if value.data.is_null() || !(0..=1024 * 1024).contains(&value.size) {
            String::new()
        } else {
            String::from_utf16_lossy(unsafe {
                std::slice::from_raw_parts(value.data, value.size as usize)
            })
        };
        unsafe { (self.string_destructor)(&mut value) };
        result
    }

    fn icon_style(icon: i32) -> u32 {
        match icon {
            1 => 0x40,
            2 => 0x30,
            3 => 0x10,
            4 => 0x20,
            _ => 0,
        }
    }

    fn buttons(mask: i32) -> Option<Vec<DialogButton>> {
        let known_mask = 0x0ffffc00;
        if mask & !known_mask != 0 {
            return None;
        }
        let mut result = Vec::new();
        for (value, label) in [
            (0x00000400, "OK"),
            (0x00000800, "Save"),
            (0x00001000, "Save All"),
            (0x00002000, "Open"),
            (0x00004000, "Yes"),
            (0x00008000, "Yes to All"),
            (0x00010000, "No"),
            (0x00020000, "No to All"),
            (0x00040000, "Abort"),
            (0x00080000, "Retry"),
            (0x00100000, "Ignore"),
            (0x00200000, "Close"),
            (0x00400000, "Cancel"),
            (0x00800000, "Discard"),
            (0x01000000, "Help"),
            (0x02000000, "Apply"),
            (0x04000000, "Reset"),
            (0x08000000, "Restore Defaults"),
        ] {
            if mask & value != 0 {
                result.push(DialogButton {
                    result: value,
                    label: label.into(),
                });
            }
        }
        if result.is_empty() {
            result.push(DialogButton {
                result: 0x400,
                label: "OK".into(),
            });
        }
        Some(result)
    }

    fn escape(buttons: &[DialogButton]) -> Option<i32> {
        [0x00400000, 0x00010000]
            .into_iter()
            .find(|result| buttons.iter().any(|button| button.result == *result))
            .or_else(|| (buttons.len() == 1).then_some(buttons[0].result))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn decodes_qt_standard_buttons() {
        let buttons = QtApi::buttons(0x00004000 | 0x00010000 | 0x00400000).unwrap();
        assert_eq!(
            buttons
                .iter()
                .map(|button| button.result)
                .collect::<Vec<_>>(),
            [0x00004000, 0x00010000, 0x00400000]
        );
        assert_eq!(QtApi::escape(&buttons), Some(0x00400000));
    }
}
