use super::*;
impl VirtualDxgiSwapchain {
    pub(super) unsafe extern "system" fn query(
        this: *mut c_void,
        iid: *const GUID,
        output: *mut *mut c_void,
    ) -> HRESULT {
        if output.is_null() || iid.is_null() {
            return E_POINTER;
        }
        unsafe {
            output.write(std::ptr::null_mut());
        }
        let iid = unsafe { *iid };
        let supported = [
            IUnknown::IID,
            IDXGIObject::IID,
            IDXGIDeviceSubObject::IID,
            IDXGISwapChain::IID,
            IDXGISwapChain1::IID,
            IDXGISwapChain2::IID,
            IDXGISwapChain3::IID,
        ];
        if !supported.contains(&iid) {
            return E_NOINTERFACE;
        }
        unsafe {
            Self::add_ref(this);
            output.write(this);
        }
        S_OK
    }
    pub(super) unsafe extern "system" fn add_ref(this: *mut c_void) -> u32 {
        let object = unsafe { Self::from_this(this) };
        object.references.fetch_add(1, Ordering::Relaxed) + 1
    }
    pub(super) unsafe extern "system" fn release(this: *mut c_void) -> u32 {
        let object = unsafe { Self::from_this(this) };
        let count = object.references.fetch_sub(1, Ordering::Release) - 1;
        if count == 0 {
            std::sync::atomic::fence(Ordering::Acquire);
            // The producer driver's Drop owns retirement; releasing this COM
            // frontend does not by itself authorize destruction of GPU work.
            let _ = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| unsafe {
                let object = Box::from_raw(this.cast::<Self>());
                object.driver.destroyed();
            }));
        }
        count
    }
    #[allow(clippy::arc_with_non_send_sync)]
    pub(super) unsafe extern "system" fn set_private(
        this: *mut c_void,
        name: *const GUID,
        size: u32,
        data: *const c_void,
    ) -> HRESULT {
        if name.is_null() || (size != 0 && data.is_null()) {
            return E_POINTER;
        }
        let object = unsafe { Self::from_this(this) };
        let name = unsafe { *name };
        if size == 0 {
            object.private.remove(&name);
            return S_OK;
        }
        let bytes =
            unsafe { std::slice::from_raw_parts(data.cast::<u8>(), size as usize) }.to_vec();
        object
            .private
            .insert(name, Arc::new(PrivateData::Bytes(bytes)));
        S_OK
    }
    #[allow(clippy::arc_with_non_send_sync)]
    pub(super) unsafe extern "system" fn set_private_interface(
        this: *mut c_void,
        name: *const GUID,
        unknown: *mut c_void,
    ) -> HRESULT {
        if name.is_null() {
            return E_POINTER;
        }
        let object = unsafe { Self::from_this(this) };
        let name = unsafe { *name };
        let Some(unknown) = (unsafe { IUnknown::from_raw_borrowed(&unknown) }) else {
            object.private.remove(&name);
            return S_OK;
        };
        let value = Arc::new(PrivateData::Interface(unknown.clone()));
        object.private.insert(name, value);
        S_OK
    }
    pub(super) unsafe extern "system" fn get_private(
        this: *mut c_void,
        name: *const GUID,
        size: *mut u32,
        data: *mut c_void,
    ) -> HRESULT {
        if name.is_null() || size.is_null() {
            return E_POINTER;
        }
        let object = unsafe { Self::from_this(this) };
        let value = object
            .private
            .get(unsafe { &*name })
            .map(|value| value.value().clone());
        let Some(value) = value else {
            unsafe {
                size.write(0);
            }
            return DXGI_ERROR_NOT_FOUND;
        };
        let required = match &*value {
            PrivateData::Bytes(bytes) => bytes.len() as u32,
            PrivateData::Interface(_) => std::mem::size_of::<*mut c_void>() as u32,
        };
        let capacity = unsafe { *size };
        unsafe {
            size.write(required);
        }
        if data.is_null() {
            return S_OK;
        }
        if capacity < required {
            return DXGI_ERROR_MORE_DATA;
        }
        match &*value {
            PrivateData::Bytes(bytes) => unsafe {
                std::ptr::copy_nonoverlapping(bytes.as_ptr(), data.cast(), bytes.len());
            },
            PrivateData::Interface(unknown) => unsafe {
                data.cast::<*mut c_void>().write(unknown.clone().into_raw());
            },
        }
        S_OK
    }
    pub(super) unsafe extern "system" fn parent(
        this: *mut c_void,
        iid: *const GUID,
        output: *mut *mut c_void,
    ) -> HRESULT {
        if iid.is_null() || output.is_null() {
            return E_POINTER;
        }
        let object = unsafe { Self::from_this(this) };
        unsafe { object.parent.query(iid, output) }
    }
    pub(super) unsafe extern "system" fn device(
        this: *mut c_void,
        iid: *const GUID,
        output: *mut *mut c_void,
    ) -> HRESULT {
        if iid.is_null() || output.is_null() {
            return E_POINTER;
        }
        let object = unsafe { Self::from_this(this) };
        unsafe { object.device.query(iid, output) }
    }
}
