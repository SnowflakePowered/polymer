//! Runtime-resolved GTK 3 `GtkMessageDialog` interception.

use polymer_ipc::ui::{DialogButton, MessageBoxRequest, UI_PROTOCOL_VERSION};
use std::{
    ffi::{CStr, c_void},
    sync::OnceLock,
};
use windows_sys::Win32::{
    Foundation::HMODULE,
    System::{
        LibraryLoader::{GetModuleHandleW, GetProcAddress},
        Threading::{GetCurrentProcessId, GetCurrentThreadId},
    },
};

type DialogRunFn = unsafe extern "C" fn(*mut c_void) -> i32;
type GetTypeFn = unsafe extern "C" fn() -> usize;
type IsTypeFn = unsafe extern "C" fn(*mut c_void, usize) -> i32;
type ObjectGetFn = unsafe extern "C" fn(*mut c_void, *const u8, ...);
type FreeFn = unsafe extern "C" fn(*mut c_void);
type WindowGetTitleFn = unsafe extern "C" fn(*mut c_void) -> *const u8;
type WindowGetDefaultWidgetFn = unsafe extern "C" fn(*mut c_void) -> *mut c_void;
type DialogGetWidgetForResponseFn = unsafe extern "C" fn(*mut c_void, i32) -> *mut c_void;
type DialogGetResponseForWidgetFn = unsafe extern "C" fn(*mut c_void, *mut c_void) -> i32;
type ButtonGetLabelFn = unsafe extern "C" fn(*mut c_void) -> *const u8;
type DialogResponseFn = unsafe extern "C" fn(*mut c_void, i32);

struct GtkApi {
    dialog_run: DialogRunFn,
    message_dialog_type: usize,
    is_type: IsTypeFn,
    object_get: ObjectGetFn,
    free: FreeFn,
    window_get_title: WindowGetTitleFn,
    window_get_default_widget: WindowGetDefaultWidgetFn,
    dialog_get_widget_for_response: DialogGetWidgetForResponseFn,
    dialog_get_response_for_widget: DialogGetResponseForWidgetFn,
    button_get_label: ButtonGetLabelFn,
    dialog_response: DialogResponseFn,
}

static API: OnceLock<GtkApi> = OnceLock::new();

const GTK3: &[u16] = &[
    b'l' as u16,
    b'i' as u16,
    b'b' as u16,
    b'g' as u16,
    b't' as u16,
    b'k' as u16,
    b'-' as u16,
    b'3' as u16,
    b'-' as u16,
    b'0' as u16,
    b'.' as u16,
    b'd' as u16,
    b'l' as u16,
    b'l' as u16,
    0,
];
const GOBJECT: &[u16] = &[
    b'l' as u16,
    b'i' as u16,
    b'b' as u16,
    b'g' as u16,
    b'o' as u16,
    b'b' as u16,
    b'j' as u16,
    b'e' as u16,
    b'c' as u16,
    b't' as u16,
    b'-' as u16,
    b'2' as u16,
    b'.' as u16,
    b'0' as u16,
    b'-' as u16,
    b'0' as u16,
    b'.' as u16,
    b'd' as u16,
    b'l' as u16,
    b'l' as u16,
    0,
];
const GLIB: &[u16] = &[
    b'l' as u16,
    b'i' as u16,
    b'b' as u16,
    b'g' as u16,
    b'l' as u16,
    b'i' as u16,
    b'b' as u16,
    b'-' as u16,
    b'2' as u16,
    b'.' as u16,
    b'0' as u16,
    b'-' as u16,
    b'0' as u16,
    b'.' as u16,
    b'd' as u16,
    b'l' as u16,
    b'l' as u16,
    0,
];

pub(crate) fn install() {
    let gtk = unsafe { GetModuleHandleW(GTK3.as_ptr()) };
    let gobject = unsafe { GetModuleHandleW(GOBJECT.as_ptr()) };
    let glib = unsafe { GetModuleHandleW(GLIB.as_ptr()) };
    if gtk.is_null() || gobject.is_null() || glib.is_null() {
        return;
    }
    let Some(api) = (unsafe { GtkApi::load(gtk, gobject, glib) }) else {
        return;
    };
    if API.set(api).is_ok() {
        let _ = super::hooks::GtkHookInstallation::install();
    }
}

impl GtkApi {
    unsafe fn load(gtk: HMODULE, gobject: HMODULE, glib: HMODULE) -> Option<Self> {
        let get_message_dialog_type: GetTypeFn =
            unsafe { Self::symbol(gtk, c"gtk_message_dialog_get_type") }?;
        Some(Self {
            dialog_run: unsafe { Self::symbol(gtk, c"gtk_dialog_run") }?,
            message_dialog_type: unsafe { get_message_dialog_type() },
            is_type: unsafe { Self::symbol(gobject, c"g_type_check_instance_is_a") }?,
            object_get: unsafe { Self::symbol(gobject, c"g_object_get") }?,
            free: unsafe { Self::symbol(glib, c"g_free") }?,
            window_get_title: unsafe { Self::symbol(gtk, c"gtk_window_get_title") }?,
            window_get_default_widget: unsafe {
                Self::symbol(gtk, c"gtk_window_get_default_widget")
            }?,
            dialog_get_widget_for_response: unsafe {
                Self::symbol(gtk, c"gtk_dialog_get_widget_for_response")
            }?,
            dialog_get_response_for_widget: unsafe {
                Self::symbol(gtk, c"gtk_dialog_get_response_for_widget")
            }?,
            button_get_label: unsafe { Self::symbol(gtk, c"gtk_button_get_label") }?,
            dialog_response: unsafe { Self::symbol(gtk, c"gtk_dialog_response") }?,
        })
    }

    unsafe fn symbol<T: Copy>(module: HMODULE, name: &CStr) -> Option<T> {
        let address = unsafe { GetProcAddress(module, name.as_ptr().cast()) }?;
        Some(unsafe { std::mem::transmute_copy(&address) })
    }

    unsafe fn forward(&self, dialog: *mut c_void) -> Option<i32> {
        let buttons = unsafe { self.buttons(dialog) };
        if buttons.is_empty() {
            return None;
        }
        let default_result = unsafe {
            let widget = (self.window_get_default_widget)(dialog);
            (!widget.is_null()).then(|| (self.dialog_get_response_for_widget)(dialog, widget))
        };
        let text = unsafe { self.property_string(dialog, c"text") };
        let secondary = unsafe { self.property_string(dialog, c"secondary-text") };
        let message_type = unsafe { self.property_i32(dialog, c"message-type") };
        let text = [text, secondary]
            .into_iter()
            .filter(|text| !text.is_empty())
            .collect::<Vec<_>>()
            .join("\n\n");
        let request = MessageBoxRequest {
            protocol_version: UI_PROTOCOL_VERSION,
            process_id: unsafe { GetCurrentProcessId() },
            thread_id: unsafe { GetCurrentThreadId() },
            owner_window: 0,
            caption: unsafe { self.borrowed_string((self.window_get_title)(dialog)) },
            text,
            style: Self::icon_style(message_type),
            default_result: default_result.or_else(|| buttons.first().map(|button| button.result)),
            escape_result: Self::escape(&buttons),
            buttons,
        };
        let session = polymer_hook::config::session().ok()?;
        polymer_ui::forward_message_box(&session, &request)
    }

    unsafe fn property_string(&self, object: *mut c_void, property: &CStr) -> String {
        let mut value = std::ptr::null_mut::<u8>();
        unsafe {
            (self.object_get)(
                object,
                property.as_ptr().cast(),
                &mut value,
                std::ptr::null::<u8>(),
            )
        };
        let result = unsafe { self.borrowed_string(value) };
        if !value.is_null() {
            unsafe { (self.free)(value.cast()) };
        }
        result
    }

    unsafe fn property_i32(&self, object: *mut c_void, property: &CStr) -> i32 {
        let mut value = 0;
        unsafe {
            (self.object_get)(
                object,
                property.as_ptr().cast(),
                &mut value,
                std::ptr::null::<u8>(),
            )
        };
        value
    }

    unsafe fn borrowed_string(&self, value: *const u8) -> String {
        if value.is_null() {
            String::new()
        } else {
            unsafe { CStr::from_ptr(value.cast()) }
                .to_string_lossy()
                .into_owned()
        }
    }

    unsafe fn buttons(&self, dialog: *mut c_void) -> Vec<DialogButton> {
        let mut buttons = Vec::new();
        for response in -11..=-1 {
            let widget = unsafe { (self.dialog_get_widget_for_response)(dialog, response) };
            if widget.is_null() {
                continue;
            }
            let label = unsafe { self.borrowed_string((self.button_get_label)(widget)) };
            buttons.push(DialogButton {
                result: response,
                label: Self::clean_label(&label),
            });
        }
        buttons
    }

    fn clean_label(label: &str) -> String {
        label
            .replace('_', "")
            .trim_start_matches("gtk-")
            .replace('-', " ")
    }

    fn icon_style(message_type: i32) -> u32 {
        match message_type {
            0 => 0x40,
            1 => 0x20,
            2 => 0x30,
            3 => 0x10,
            _ => 0,
        }
    }

    fn escape(buttons: &[DialogButton]) -> Option<i32> {
        [-6, -7, -9]
            .into_iter()
            .find(|response| buttons.iter().any(|button| button.result == *response))
            .or_else(|| (buttons.len() == 1).then_some(buttons[0].result))
    }
}

pub(super) fn dialog_run_target() -> DialogRunFn {
    API.get().expect("GTK API was loaded").dialog_run
}

pub(super) unsafe fn dialog_run(
    dialog: *mut c_void,
    mut context: super::hooks::GtkDialogRunContext<'_>,
) -> i32 {
    let Some(api) = API.get() else {
        return polymer_hook::next!(context, dialog);
    };
    if unsafe { (api.is_type)(dialog, api.message_dialog_type) } == 0 {
        return polymer_hook::next!(context, dialog);
    }
    let Some(response) = (unsafe { api.forward(dialog) }) else {
        return polymer_hook::next!(context, dialog);
    };
    unsafe { (api.dialog_response)(dialog, response) };
    response
}

