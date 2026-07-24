#![allow(unsafe_code)]
#![allow(non_snake_case)]

use crate::{UserEvent, composition::SurfaceLayout};
use windows::{
    Win32::{
        Foundation::{
            DISP_E_BADPARAMCOUNT, DISP_E_MEMBERNOTFOUND, DISP_E_TYPEMISMATCH, DISP_E_UNKNOWNNAME,
            E_NOTIMPL, S_OK,
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

const DISPID_SET_VIEWPORTS: i32 = 1;

#[interface("9A434707-9362-4D43-B641-1B86C1821C32")]
pub unsafe trait IGameViewHost: IDispatch {
    fn SetViewports(&self, json: PCWSTR) -> HRESULT;
}

#[implement(IGameViewHost)]
struct GameViewHost {
    proxy: EventLoopProxy<UserEvent>,
}

impl GameViewHost {
    fn apply_layout(&self, json: &str) -> HRESULT {
        match serde_json::from_str::<Vec<SurfaceLayout>>(json) {
            Ok(layouts) => match self.proxy.send_event(UserEvent::Layout(layouts)) {
                Ok(()) => S_OK,
                Err(_) => E_NOTIMPL,
            },
            Err(error) => {
                eprintln!("snowglass: invalid WebView2 surface layout: {error}");
                DISP_E_TYPEMISMATCH
            }
        }
    }
}

#[allow(non_snake_case)]
impl IGameViewHost_Impl for GameViewHost_Impl {
    unsafe fn SetViewports(&self, json: PCWSTR) -> HRESULT {
        // SAFETY: WebView2 supplies a valid null-terminated string for the
        // duration of this COM call.
        match unsafe { json.to_string() } {
            Ok(json) => self.apply_layout(&json),
            Err(_) => DISP_E_TYPEMISMATCH,
        }
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
            // SAFETY: COM guarantees arrays containing `count` entries.
            let name = unsafe { *names.add(index) };
            let name = unsafe { name.to_string() }.unwrap_or_default();
            let id = if name.eq_ignore_ascii_case("setViewports") {
                DISPID_SET_VIEWPORTS
            } else {
                unknown = true;
                DISPID_UNKNOWN
            };
            // SAFETY: `ids` is the caller-provided output array.
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
        if member != DISPID_SET_VIEWPORTS || flags.0 & DISPATCH_METHOD.0 == 0 {
            return Err(Error::from_hresult(DISP_E_MEMBERNOTFOUND));
        }
        if parameters.is_null() {
            return Err(Error::from_hresult(DISP_E_BADPARAMCOUNT));
        }
        // SAFETY: WebView2 supplies a valid DISPPARAMS for this call.
        let parameters = unsafe { &*parameters };
        if parameters.cArgs != 1 || parameters.rgvarg.is_null() {
            return Err(Error::from_hresult(DISP_E_BADPARAMCOUNT));
        }
        // Automation arguments are stored in reverse order; there is one.
        let value = unsafe { &*parameters.rgvarg };
        let json = BSTR::try_from(value).map_err(|_| {
            if !argument_error.is_null() {
                // SAFETY: this is the optional caller-provided error index.
                unsafe { argument_error.write(0) };
            }
            Error::from_hresult(DISP_E_TYPEMISMATCH)
        })?;
        let status = self.apply_layout(&json.to_string());
        status.ok()?;
        if !result.is_null() {
            // SAFETY: this is the optional caller-provided return value.
            unsafe { result.write(VARIANT::default()) };
        }
        Ok(())
    }
}

pub fn create(proxy: EventLoopProxy<UserEvent>) -> IGameViewHost {
    GameViewHost { proxy }.into()
}
