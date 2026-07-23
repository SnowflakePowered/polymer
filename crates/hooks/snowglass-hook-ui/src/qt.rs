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
type QStringUtf16Fn = unsafe extern "C" fn(*const QtString) -> *const u16;
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
    // QString contains a QArrayDataPointer<char16_t>: allocation, data, size.
    storage: [usize; 3],
}

struct QtApi {
    widgets: usize,
    inherits: InheritsFn,
    utf16: QStringUtf16Fn,
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

const DIALOG_EXEC: &CStr = c"?exec@QDialog@@UEAAHXZ";
const INHERITS: &CStr = c"?inherits@QObject@@QEBA_NPEBD@Z";
const QSTRING_UTF16: &CStr = c"?utf16@QString@@QEBAPEBGXZ";
const QSTRING_DESTRUCTOR: &CStr = c"??1QString@@QEAA@XZ";
const WINDOW_TITLE: &CStr = c"?windowTitle@QWidget@@QEBA?AVQString@@XZ";
const TEXT: &CStr = c"?text@QMessageBox@@QEBA?AVQString@@XZ";
const INFORMATIVE_TEXT: &CStr = c"?informativeText@QMessageBox@@QEBA?AVQString@@XZ";
const DETAILED_TEXT: &CStr = c"?detailedText@QMessageBox@@QEBA?AVQString@@XZ";
const STANDARD_BUTTONS: &CStr =
    c"?standardButtons@QMessageBox@@QEBA?AV?$QFlags@W4StandardButton@QMessageBox@@@@XZ";
const ICON: &CStr = c"?icon@QMessageBox@@QEBA?AW4Icon@1@XZ";
const DEFAULT_BUTTON: &CStr = c"?defaultButton@QMessageBox@@QEBAPEAVQPushButton@@XZ";
const ESCAPE_BUTTON: &CStr = c"?escapeButton@QMessageBox@@QEBAPEAVQAbstractButton@@XZ";
const STANDARD_BUTTON: &CStr =
    c"?standardButton@QMessageBox@@QEBA?AW4StandardButton@1@PEAVQAbstractButton@@@Z";
const BUTTON: &CStr = c"?button@QMessageBox@@QEBAPEAVQAbstractButton@@W4StandardButton@1@@Z";
const CLICK: &CStr = c"?click@QAbstractButton@@QEAAXXZ";

snowglass_hook::chain_hooks! {
    #![order(reverse)]
    #![detour(snowglass_platform_windows::InlineDetour<_>)]
    #[installer]
    pub(crate) struct QtHookInstallation;

    #[trampoline(abi = "C")]
    #[terminal(installed(target = dialog_exec_target()), fallback = 0)]
    #[infix(dialog_exec)]
    unsafe extern "C" fn qt_dialog_exec(dialog: *mut c_void) -> i32;
}

pub(crate) fn install() {
    let widgets = unsafe { GetModuleHandleW(QT6_WIDGETS.as_ptr()) };
    let core = unsafe { GetModuleHandleW(QT6_CORE.as_ptr()) };
    if widgets.is_null() || core.is_null() {
        return;
    }
    let Some(api) = (unsafe { load_api(widgets, core) }) else {
        return;
    };
    if API.set(api).is_ok() {
        let _ = QtHookInstallation::install();
    }
}

unsafe fn load_api(widgets: HMODULE, core: HMODULE) -> Option<QtApi> {
    Some(QtApi {
        widgets: widgets as usize,
        inherits: unsafe { symbol(core, INHERITS) }?,
        utf16: unsafe { symbol(core, QSTRING_UTF16) }?,
        string_destructor: unsafe { symbol(core, QSTRING_DESTRUCTOR) }?,
        window_title: unsafe { symbol(widgets, WINDOW_TITLE) }?,
        text: unsafe { symbol(widgets, TEXT) }?,
        informative_text: unsafe { symbol(widgets, INFORMATIVE_TEXT) }?,
        detailed_text: unsafe { symbol(widgets, DETAILED_TEXT) }?,
        standard_buttons: unsafe { symbol(widgets, STANDARD_BUTTONS) }?,
        icon: unsafe { symbol(widgets, ICON) }?,
        default_button: unsafe { symbol(widgets, DEFAULT_BUTTON) }?,
        escape_button: unsafe { symbol(widgets, ESCAPE_BUTTON) }?,
        standard_button: unsafe { symbol(widgets, STANDARD_BUTTON) }?,
        button: unsafe { symbol(widgets, BUTTON) }?,
        click: unsafe { symbol(widgets, CLICK) }?,
    })
}

fn dialog_exec_target() -> DialogExecFn {
    let api = API.get().expect("Qt API was loaded");
    unsafe { symbol(api.widgets as HMODULE, DIALOG_EXEC) }
        .expect("Qt dialog exec export was verified")
}

unsafe fn symbol<T: Copy>(module: HMODULE, name: &CStr) -> Option<T> {
    let address = unsafe { GetProcAddress(module, name.as_ptr().cast()) }?;
    // SAFETY: each caller supplies the ABI matching the named Qt export.
    Some(unsafe { std::mem::transmute_copy(&address) })
}

unsafe fn dialog_exec(dialog: *mut c_void, mut context: QtDialogExecContext<'_>) -> i32 {
    let Some(api) = API.get() else {
        return snowglass_hook::next!(context, dialog);
    };
    if unsafe { (api.inherits)(dialog, c"QMessageBox".as_ptr().cast()) } == 0 {
        return snowglass_hook::next!(context, dialog);
    }
    let Some(result) = (unsafe { forward(api, dialog) }) else {
        return snowglass_hook::next!(context, dialog);
    };
    let button = unsafe { (api.button)(dialog, result) };
    if !button.is_null() {
        // This preserves QMessageBox::clickedButton(), result(), and buttonClicked.
        unsafe { (api.click)(button) };
    }
    result
}

unsafe fn forward(api: &QtApi, dialog: *mut c_void) -> Option<i32> {
    let button_mask = unsafe { get_flags(api.standard_buttons, dialog) };
    let buttons = qt_buttons(button_mask)?;
    let default_button = unsafe { standard_button(api, dialog, (api.default_button)(dialog)) };
    let explicit_escape = unsafe { standard_button(api, dialog, (api.escape_button)(dialog)) };
    let primary = unsafe { get_string(api, api.text, dialog) };
    let informative = unsafe { get_string(api, api.informative_text, dialog) };
    let details = unsafe { get_string(api, api.detailed_text, dialog) };
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
        caption: unsafe { get_string(api, api.window_title, dialog) },
        text,
        style: qt_icon_style(unsafe { (api.icon)(dialog) }),
        default_result: default_button.or_else(|| buttons.first().map(|button| button.result)),
        escape_result: explicit_escape.or_else(|| qt_escape(&buttons)),
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

unsafe fn standard_button(api: &QtApi, dialog: *mut c_void, button: *mut c_void) -> Option<i32> {
    if button.is_null() {
        None
    } else {
        let result = unsafe { (api.standard_button)(dialog, button) };
        (result != 0).then_some(result)
    }
}

unsafe fn get_string(api: &QtApi, getter: StringGetterFn, object: *mut c_void) -> String {
    let mut value = MaybeUninit::<QtString>::uninit();
    unsafe { getter(object, value.as_mut_ptr()) };
    let mut value = unsafe { value.assume_init() };
    let pointer = unsafe { (api.utf16)(&value) };
    let mut length = 0;
    if !pointer.is_null() {
        while length < 1024 * 1024 && unsafe { *pointer.add(length) } != 0 {
            length += 1;
        }
    }
    let result = if pointer.is_null() {
        String::new()
    } else {
        String::from_utf16_lossy(unsafe { std::slice::from_raw_parts(pointer, length) })
    };
    unsafe { (api.string_destructor)(&mut value) };
    result
}

fn qt_icon_style(icon: i32) -> u32 {
    match icon {
        1 => 0x40,
        2 => 0x30,
        3 => 0x10,
        4 => 0x20,
        _ => 0,
    }
}

fn qt_buttons(mask: i32) -> Option<Vec<DialogButton>> {
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

fn qt_escape(buttons: &[DialogButton]) -> Option<i32> {
    [0x00400000, 0x00010000]
        .into_iter()
        .find(|result| buttons.iter().any(|button| button.result == *result))
        .or_else(|| (buttons.len() == 1).then_some(buttons[0].result))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn decodes_qt_standard_buttons() {
        let buttons = qt_buttons(0x00004000 | 0x00010000 | 0x00400000).unwrap();
        assert_eq!(
            buttons
                .iter()
                .map(|button| button.result)
                .collect::<Vec<_>>(),
            [0x00004000, 0x00010000, 0x00400000]
        );
        assert_eq!(qt_escape(&buttons), Some(0x00400000));
    }
}
