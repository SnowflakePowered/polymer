use super::*;
impl VirtualDxgiSwapchain {
    pub(super) unsafe extern "system" fn set_fullscreen(
        _: *mut c_void,
        fullscreen: windows::core::BOOL,
        target: *mut c_void,
    ) -> HRESULT {
        if fullscreen.as_bool() || !target.is_null() {
            DXGI_ERROR_UNSUPPORTED
        } else {
            S_OK
        }
    }
    pub(super) unsafe extern "system" fn fullscreen(
        _: *mut c_void,
        fullscreen: *mut windows::core::BOOL,
        output: *mut *mut c_void,
    ) -> HRESULT {
        if !fullscreen.is_null() {
            unsafe {
                fullscreen.write(windows::core::BOOL(0));
            }
        }
        if !output.is_null() {
            unsafe {
                output.write(std::ptr::null_mut());
            }
        }
        S_OK
    }
    pub(super) unsafe extern "system" fn fullscreen_desc(
        _: *mut c_void,
        output: *mut DXGI_SWAP_CHAIN_FULLSCREEN_DESC,
    ) -> HRESULT {
        if output.is_null() {
            return E_POINTER;
        }
        unsafe {
            output.write(DXGI_SWAP_CHAIN_FULLSCREEN_DESC {
                Windowed: windows::core::BOOL(1),
                ..Default::default()
            });
        }
        S_OK
    }
    pub(super) unsafe extern "system" fn hwnd(this: *mut c_void, output: *mut HWND) -> HRESULT {
        if output.is_null() {
            return E_POINTER;
        }
        unsafe {
            output.write(Self::from_this(this).window);
        }
        S_OK
    }
    pub(super) unsafe extern "system" fn core_window(
        _: *mut c_void,
        _: *const GUID,
        output: *mut *mut c_void,
    ) -> HRESULT {
        if output.is_null() {
            return E_POINTER;
        }
        unsafe {
            output.write(std::ptr::null_mut());
        }
        DXGI_ERROR_INVALID_CALL
    }
    pub(super) unsafe extern "system" fn containing_output(
        _: *mut c_void,
        output: *mut *mut c_void,
    ) -> HRESULT {
        if output.is_null() {
            return E_POINTER;
        }
        unsafe {
            output.write(std::ptr::null_mut());
        }
        DXGI_ERROR_NOT_FOUND
    }
    pub(super) unsafe extern "system" fn restrict_output(
        _: *mut c_void,
        output: *mut *mut c_void,
    ) -> HRESULT {
        if output.is_null() {
            return E_POINTER;
        }
        unsafe {
            output.write(std::ptr::null_mut());
        }
        S_OK
    }
    pub(super) unsafe extern "system" fn mono(_: *mut c_void) -> windows::core::BOOL {
        windows::core::BOOL(0)
    }
    pub(super) unsafe extern "system" fn set_background(
        this: *mut c_void,
        color: *const DXGI_RGBA,
    ) -> HRESULT {
        let Some(color) = (unsafe { color.as_ref() }) else {
            return E_POINTER;
        };
        if ![color.r, color.g, color.b, color.a]
            .iter()
            .all(|value| value.is_finite() && (0.0..=1.0).contains(value))
        {
            return E_INVALIDARG;
        }
        // Opaque stretch presentation never exposes the background, but DXGI
        // still stores it and returns the exact state to subsequent queries.
        *unsafe { Self::from_this(this) }.background.lock() = *color;
        S_OK
    }
    pub(super) unsafe extern "system" fn background(
        this: *mut c_void,
        output: *mut DXGI_RGBA,
    ) -> HRESULT {
        if output.is_null() {
            return E_POINTER;
        }
        unsafe {
            output.write(*Self::from_this(this).background.lock());
        }
        S_OK
    }
    pub(super) unsafe extern "system" fn set_rotation(
        _: *mut c_void,
        rotation: DXGI_MODE_ROTATION,
    ) -> HRESULT {
        if rotation == DXGI_MODE_ROTATION_IDENTITY {
            S_OK
        } else {
            DXGI_ERROR_INVALID_CALL
        }
    }
    pub(super) unsafe extern "system" fn rotation(
        _: *mut c_void,
        output: *mut DXGI_MODE_ROTATION,
    ) -> HRESULT {
        if output.is_null() {
            return E_POINTER;
        }
        unsafe {
            output.write(DXGI_MODE_ROTATION_IDENTITY);
        }
        S_OK
    }
    pub(super) unsafe extern "system" fn set_source(
        this: *mut c_void,
        width: u32,
        height: u32,
    ) -> HRESULT {
        let desc = unsafe { Self::from_this(this) }.description;
        if width == desc.Width && height == desc.Height {
            S_OK
        } else {
            DXGI_ERROR_INVALID_CALL
        }
    }
    pub(super) unsafe extern "system" fn source(
        this: *mut c_void,
        width: *mut u32,
        height: *mut u32,
    ) -> HRESULT {
        if width.is_null() || height.is_null() {
            return E_POINTER;
        }
        let desc = unsafe { Self::from_this(this) }.description;
        unsafe {
            width.write(desc.Width);
            height.write(desc.Height);
        }
        S_OK
    }
    pub(super) unsafe extern "system" fn set_latency(this: *mut c_void, latency: u32) -> HRESULT {
        let object = unsafe { Self::from_this(this) };
        if object.description.Flags & DXGI_SWAP_CHAIN_FLAG_FRAME_LATENCY_WAITABLE_OBJECT.0 as u32
            == 0
        {
            return DXGI_ERROR_INVALID_CALL;
        }
        if latency == 0 || latency > 16 {
            return E_INVALIDARG;
        }
        Self::guarded(|| object.driver.set_maximum_latency(latency))
    }
    pub(super) unsafe extern "system" fn latency(this: *mut c_void, output: *mut u32) -> HRESULT {
        if output.is_null() {
            return E_POINTER;
        }
        let object = unsafe { Self::from_this(this) };
        if object.description.Flags & DXGI_SWAP_CHAIN_FLAG_FRAME_LATENCY_WAITABLE_OBJECT.0 as u32
            == 0
        {
            return DXGI_ERROR_INVALID_CALL;
        }
        unsafe {
            output.write(object.driver.maximum_latency());
        }
        S_OK
    }
    pub(super) unsafe extern "system" fn latency_handle(this: *mut c_void) -> HANDLE {
        let object = unsafe { Self::from_this(this) };
        if object.description.Flags & DXGI_SWAP_CHAIN_FLAG_FRAME_LATENCY_WAITABLE_OBJECT.0 as u32
            == 0
        {
            return HANDLE::default();
        }
        object.driver.latency_handle()
    }
    pub(super) unsafe extern "system" fn set_matrix(
        _: *mut c_void,
        _: *const DXGI_MATRIX_3X2_F,
    ) -> HRESULT {
        DXGI_ERROR_INVALID_CALL
    }
    pub(super) unsafe extern "system" fn matrix(
        _: *mut c_void,
        _: *mut DXGI_MATRIX_3X2_F,
    ) -> HRESULT {
        DXGI_ERROR_INVALID_CALL
    }
    pub(super) unsafe extern "system" fn color_support(
        _: *mut c_void,
        color: DXGI_COLOR_SPACE_TYPE,
        output: *mut u32,
    ) -> HRESULT {
        if output.is_null() {
            return E_POINTER;
        }
        let support = if color == DXGI_COLOR_SPACE_RGB_FULL_G22_NONE_P709 {
            DXGI_SWAP_CHAIN_COLOR_SPACE_SUPPORT_FLAG_PRESENT.0 as u32
        } else {
            0
        };
        unsafe {
            output.write(support);
        }
        S_OK
    }
    pub(super) unsafe extern "system" fn set_color(
        _: *mut c_void,
        color: DXGI_COLOR_SPACE_TYPE,
    ) -> HRESULT {
        if color == DXGI_COLOR_SPACE_RGB_FULL_G22_NONE_P709 {
            S_OK
        } else {
            E_INVALIDARG
        }
    }
}
