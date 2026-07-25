#![allow(unsafe_code)]
#![allow(non_snake_case)]

use crate::{UserEvent, api::LaunchOptions};
use std::sync::atomic::{AtomicU64, Ordering};
use windows::{
    Win32::{
        Foundation::{
            DISP_E_BADPARAMCOUNT, DISP_E_MEMBERNOTFOUND, DISP_E_TYPEMISMATCH, DISP_E_UNKNOWNNAME,
            E_FAIL, E_NOTIMPL, S_OK,
        },
        System::{
            Com::{
                DISPATCH_FLAGS, DISPATCH_METHOD, DISPPARAMS, EXCEPINFO, IDispatch, IDispatch_Impl,
                IDispatch_Vtbl, ITypeInfo,
            },
            Ole::DISPID_UNKNOWN,
            Variant::VARIANT,
        },
    },
    core::{BSTR, Error, GUID, HRESULT, PCWSTR, Result},
};
use windows_core::{implement, interface};
use winit::event_loop::EventLoopProxy;

const DISPID_LAUNCH: i32 = 1;
const DISPID_CLOSE: i32 = 2;
const DISPID_MESSAGE: i32 = 3;
const DISPID_ACTIVATE_INPUT: i32 = 4;
const DISPID_DEACTIVATE_INPUT: i32 = 5;

#[interface("9A434707-9362-4D43-B641-1B86C1821C32")]
pub unsafe trait IGameViewHost: IDispatch {
    fn Launch(&self, json: PCWSTR, instance_id: *mut BSTR) -> HRESULT;
    fn Close(&self, instance_id: PCWSTR) -> HRESULT;
    fn Message(&self, json: PCWSTR) -> HRESULT;
    fn ActivateInput(&self) -> HRESULT;
    fn DeactivateInput(&self) -> HRESULT;
}

#[implement(IGameViewHost)]
struct GameViewHost {
    proxy: EventLoopProxy<UserEvent>,
    next_instance: AtomicU64,
}

#[derive(serde::Deserialize)]
#[serde(rename_all = "camelCase")]
struct MessageRequest {
    instance_id: String,
    name: String,
    args: serde_json::Value,
}

impl GameViewHost {
    fn launch(&self, json: &str) -> Result<String> {
        let options = serde_json::from_str::<LaunchOptions>(json)
            .map_err(|error| invalid_argument(format!("invalid LaunchOptions: {error}")))?;
        options.validate().map_err(invalid_argument)?;
        let instance_id = self
            .next_instance
            .fetch_add(1, Ordering::Relaxed)
            .to_string();
        self.proxy
            .send_event(UserEvent::Launch {
                instance_id: instance_id.clone(),
                options,
            })
            .map_err(|_| Error::from_hresult(E_FAIL))?;
        Ok(instance_id)
    }

    fn close(&self, instance_id: &str) -> Result<()> {
        if instance_id.is_empty() {
            return Err(invalid_argument("instance ID must not be empty"));
        }
        self.proxy
            .send_event(UserEvent::CloseInstance(instance_id.to_owned()))
            .map_err(|_| Error::from_hresult(E_FAIL))
    }

    fn message(&self, json: &str) -> Result<()> {
        let request = serde_json::from_str::<MessageRequest>(json)
            .map_err(|error| invalid_argument(format!("invalid game message: {error}")))?;
        self.proxy
            .send_event(UserEvent::GameMessage {
                instance_id: request.instance_id,
                name: request.name,
                args: request.args,
            })
            .map_err(|_| Error::from_hresult(E_FAIL))
    }

    fn set_input(&self, active: bool) -> Result<()> {
        self.proxy
            .send_event(UserEvent::InputPriority(if active {
                crate::InputPriority::Game
            } else {
                crate::InputPriority::Foreground
            }))
            .map_err(|_| Error::from_hresult(E_FAIL))
    }
}

#[allow(non_snake_case)]
impl IGameViewHost_Impl for GameViewHost_Impl {
    unsafe fn Launch(&self, json: PCWSTR, instance_id: *mut BSTR) -> HRESULT {
        if instance_id.is_null() {
            return DISP_E_TYPEMISMATCH;
        }
        let result = unsafe { json.to_string() }
            .map_err(|_| Error::from_hresult(DISP_E_TYPEMISMATCH))
            .and_then(|json| self.launch(&json));
        match result {
            Ok(id) => {
                unsafe { instance_id.write(BSTR::from(id)) };
                S_OK
            }
            Err(error) => error.code(),
        }
    }

    unsafe fn Close(&self, instance_id: PCWSTR) -> HRESULT {
        unsafe { instance_id.to_string() }
            .map_err(|_| Error::from_hresult(DISP_E_TYPEMISMATCH))
            .and_then(|id| self.close(&id))
            .map_or_else(|error| error.code(), |()| S_OK)
    }

    unsafe fn Message(&self, json: PCWSTR) -> HRESULT {
        unsafe { json.to_string() }
            .map_err(|_| Error::from_hresult(DISP_E_TYPEMISMATCH))
            .and_then(|json| self.message(&json))
            .map_or_else(|error| error.code(), |()| S_OK)
    }

    unsafe fn ActivateInput(&self) -> HRESULT {
        self.set_input(true)
            .map_or_else(|error| error.code(), |()| S_OK)
    }

    unsafe fn DeactivateInput(&self) -> HRESULT {
        self.set_input(false)
            .map_or_else(|error| error.code(), |()| S_OK)
    }
}

#[allow(non_snake_case)]
impl IDispatch_Impl for GameViewHost_Impl {
    fn GetTypeInfoCount(&self) -> Result<u32> {
        Ok(0)
    }

    fn GetTypeInfo(&self, _itinfo: u32, _lcid: u32) -> Result<ITypeInfo> {
        Err(Error::from_hresult(E_NOTIMPL))
    }

    fn GetIDsOfNames(
        &self,
        _riid: *const GUID,
        names: *const PCWSTR,
        count: u32,
        _lcid: u32,
        ids: *mut i32,
    ) -> Result<()> {
        if names.is_null() || ids.is_null() || count == 0 {
            return Err(Error::from_hresult(DISP_E_UNKNOWNNAME));
        }
        let mut unknown = false;
        for index in 0..count as usize {
            let name = unsafe { *names.add(index) };
            let name = unsafe { name.to_string() }.unwrap_or_default();
            let id = if name.eq_ignore_ascii_case("launch") {
                DISPID_LAUNCH
            } else if name.eq_ignore_ascii_case("close") {
                DISPID_CLOSE
            } else if name.eq_ignore_ascii_case("message") {
                DISPID_MESSAGE
            } else if name.eq_ignore_ascii_case("activateInput") {
                DISPID_ACTIVATE_INPUT
            } else if name.eq_ignore_ascii_case("deactivateInput") {
                DISPID_DEACTIVATE_INPUT
            } else {
                unknown = true;
                DISPID_UNKNOWN
            };
            unsafe { ids.add(index).write(id) };
        }
        if unknown {
            Err(Error::from_hresult(DISP_E_UNKNOWNNAME))
        } else {
            Ok(())
        }
    }

    fn Invoke(
        &self,
        member: i32,
        _riid: *const GUID,
        _lcid: u32,
        flags: DISPATCH_FLAGS,
        parameters: *const DISPPARAMS,
        result: *mut VARIANT,
        _exception: *mut EXCEPINFO,
        argument_error: *mut u32,
    ) -> Result<()> {
        if flags.0 & DISPATCH_METHOD.0 == 0 {
            return Err(Error::from_hresult(DISP_E_MEMBERNOTFOUND));
        }
        let argument = dispatch_string_argument(parameters, argument_error)?;
        match member {
            DISPID_LAUNCH => {
                let id = self.launch(
                    argument
                        .as_deref()
                        .ok_or_else(|| Error::from_hresult(DISP_E_BADPARAMCOUNT))?,
                )?;
                if !result.is_null() {
                    unsafe { result.write(VARIANT::from(BSTR::from(id))) };
                }
            }
            DISPID_CLOSE => self.close(
                argument
                    .as_deref()
                    .ok_or_else(|| Error::from_hresult(DISP_E_BADPARAMCOUNT))?,
            )?,
            DISPID_MESSAGE => self.message(
                argument
                    .as_deref()
                    .ok_or_else(|| Error::from_hresult(DISP_E_BADPARAMCOUNT))?,
            )?,
            DISPID_ACTIVATE_INPUT => {
                require_no_arguments(parameters)?;
                self.set_input(true)?;
            }
            DISPID_DEACTIVATE_INPUT => {
                require_no_arguments(parameters)?;
                self.set_input(false)?;
            }
            _ => return Err(Error::from_hresult(DISP_E_MEMBERNOTFOUND)),
        }
        Ok(())
    }
}

fn dispatch_string_argument(
    parameters: *const DISPPARAMS,
    argument_error: *mut u32,
) -> Result<Option<String>> {
    if parameters.is_null() {
        return Ok(None);
    }
    let parameters = unsafe { &*parameters };
    if parameters.cArgs == 0 {
        return Ok(None);
    }
    if parameters.cArgs != 1 || parameters.rgvarg.is_null() {
        return Err(Error::from_hresult(DISP_E_BADPARAMCOUNT));
    }
    let value = unsafe { &*parameters.rgvarg };
    BSTR::try_from(value)
        .map(|value| Some(value.to_string()))
        .map_err(|_| {
            if !argument_error.is_null() {
                unsafe { argument_error.write(0) };
            }
            Error::from_hresult(DISP_E_TYPEMISMATCH)
        })
}

fn require_no_arguments(parameters: *const DISPPARAMS) -> Result<()> {
    if parameters.is_null() || unsafe { (*parameters).cArgs } == 0 {
        Ok(())
    } else {
        Err(Error::from_hresult(DISP_E_BADPARAMCOUNT))
    }
}

fn invalid_argument(message: impl Into<String>) -> Error {
    Error::new(DISP_E_TYPEMISMATCH, message.into())
}

pub fn create(proxy: EventLoopProxy<UserEvent>) -> IGameViewHost {
    GameViewHost {
        proxy,
        next_instance: AtomicU64::new(1),
    }
    .into()
}
