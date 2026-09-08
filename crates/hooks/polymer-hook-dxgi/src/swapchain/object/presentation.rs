use super::*;
impl VirtualDxgiSwapchain {
    pub(super) unsafe extern "system" fn present(
        this: *mut c_void,
        interval: u32,
        flags: DXGI_PRESENT,
    ) -> HRESULT {
        let object = unsafe { Self::from_this(this) };
        let allowed = DXGI_PRESENT_TEST | DXGI_PRESENT_DO_NOT_WAIT | DXGI_PRESENT_ALLOW_TEARING;
        if interval > 4 || flags.0 & !allowed.0 != 0 {
            return DXGI_ERROR_INVALID_CALL;
        }
        if flags.contains(DXGI_PRESENT_ALLOW_TEARING)
            && (interval != 0
                || object.description.Flags & DXGI_SWAP_CHAIN_FLAG_ALLOW_TEARING.0 as u32 == 0)
        {
            return DXGI_ERROR_INVALID_CALL;
        }
        let result = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
            object.driver.present(interval, flags)
        }))
        .unwrap_or(E_FAIL);
        if result == S_OK && !flags.contains(DXGI_PRESENT_TEST) {
            object.presents.fetch_add(1, Ordering::Relaxed);
        }
        result
    }
    pub(super) unsafe extern "system" fn present1(
        this: *mut c_void,
        interval: u32,
        flags: DXGI_PRESENT,
        parameters: *const DXGI_PRESENT_PARAMETERS,
    ) -> HRESULT {
        let Some(parameters) = (unsafe { parameters.as_ref() }) else {
            return E_POINTER;
        };
        // Flip-discard has no preserved dirty/scroll history to expose.
        if parameters.DirtyRectsCount != 0
            || !parameters.pScrollRect.is_null()
            || !parameters.pScrollOffset.is_null()
        {
            return DXGI_ERROR_INVALID_CALL;
        }
        unsafe { Self::present(this, interval, flags) }
    }
    pub(super) unsafe extern "system" fn buffer(
        this: *mut c_void,
        index: u32,
        iid: *const GUID,
        output: *mut *mut c_void,
    ) -> HRESULT {
        if iid.is_null() || output.is_null() {
            return E_POINTER;
        }
        unsafe {
            output.write(std::ptr::null_mut());
        }
        let object = unsafe { Self::from_this(this) };
        if index >= object.description.BufferCount {
            return DXGI_ERROR_INVALID_CALL;
        }
        Self::guarded(|| {
            let buffer = object.driver.buffer(index)?;
            unsafe { buffer.query(iid, output) }.ok()
        })
    }
    pub(super) unsafe extern "system" fn current_index(this: *mut c_void) -> u32 {
        unsafe { Self::from_this(this) }.driver.current_index()
    }
    pub(super) unsafe extern "system" fn present_count(
        this: *mut c_void,
        output: *mut u32,
    ) -> HRESULT {
        if output.is_null() {
            return E_POINTER;
        }
        unsafe {
            output.write(Self::from_this(this).presents.load(Ordering::Relaxed));
        }
        S_OK
    }
    pub(super) unsafe extern "system" fn statistics(
        _: *mut c_void,
        output: *mut DXGI_FRAME_STATISTICS,
    ) -> HRESULT {
        if output.is_null() {
            E_POINTER
        } else {
            DXGI_ERROR_FRAME_STATISTICS_DISJOINT
        }
    }
    pub(super) unsafe extern "system" fn desc(
        this: *mut c_void,
        output: *mut DXGI_SWAP_CHAIN_DESC,
    ) -> HRESULT {
        if output.is_null() {
            return E_POINTER;
        }
        let object = unsafe { Self::from_this(this) };
        let desc = object.description;
        unsafe {
            output.write(DXGI_SWAP_CHAIN_DESC {
                BufferDesc: DXGI_MODE_DESC {
                    Width: desc.Width,
                    Height: desc.Height,
                    Format: desc.Format,
                    ..Default::default()
                },
                SampleDesc: desc.SampleDesc,
                BufferUsage: desc.BufferUsage,
                BufferCount: desc.BufferCount,
                OutputWindow: object.window,
                Windowed: windows::core::BOOL(1),
                SwapEffect: desc.SwapEffect,
                Flags: desc.Flags,
            });
        }
        S_OK
    }
    pub(super) unsafe extern "system" fn desc1(
        this: *mut c_void,
        output: *mut DXGI_SWAP_CHAIN_DESC1,
    ) -> HRESULT {
        if output.is_null() {
            return E_POINTER;
        }
        unsafe {
            output.write(Self::from_this(this).description);
        }
        S_OK
    }
    pub(super) unsafe extern "system" fn resize(
        this: *mut c_void,
        buffer_count: u32,
        width: u32,
        height: u32,
        format: DXGI_FORMAT,
        flags: u32,
    ) -> HRESULT {
        let desc = unsafe { Self::from_this(this) }.description;
        let count_matches = buffer_count == 0 || buffer_count == desc.BufferCount;
        let width_matches = width == 0 || width == desc.Width;
        let height_matches = height == 0 || height == desc.Height;
        let format_matches = format == DXGI_FORMAT_UNKNOWN || format == desc.Format;
        if count_matches && width_matches && height_matches && format_matches && flags == desc.Flags
        {
            // The backing allocation is already the fixed requested profile.
            // Treat an application's resize acknowledgement as a successful
            // no-op so a host-window resize does not stall presentation.
            S_OK
        } else {
            DXGI_ERROR_INVALID_CALL
        }
    }
    pub(super) unsafe extern "system" fn resize1(
        _: *mut c_void,
        _: u32,
        _: u32,
        _: u32,
        _: DXGI_FORMAT,
        _: u32,
        _: *const u32,
        _: *const *mut c_void,
    ) -> HRESULT {
        DXGI_ERROR_INVALID_CALL
    }
    pub(super) unsafe extern "system" fn resize_target(
        this: *mut c_void,
        target: *const DXGI_MODE_DESC,
    ) -> HRESULT {
        let Some(target) = (unsafe { target.as_ref() }) else {
            return E_POINTER;
        };
        let desc = unsafe { Self::from_this(this) }.description;
        if target.Width != desc.Width
            || target.Height != desc.Height
            || (target.Format != DXGI_FORMAT_UNKNOWN && target.Format != desc.Format)
        {
            return DXGI_ERROR_INVALID_CALL;
        }
        if target.RefreshRate.Numerator != 0
            || target.ScanlineOrdering != DXGI_MODE_SCANLINE_ORDER_UNSPECIFIED
            || target.Scaling != DXGI_MODE_SCALING_UNSPECIFIED
        {
            return DXGI_ERROR_UNSUPPORTED;
        }
        S_OK
    }
}
