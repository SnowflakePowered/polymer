//! Typed chains between COM ABI trampolines, interception policy, and terminals.

use crate::loader::DirectInput8CreateFn;
pub(crate) use snowglass_hook::HookChain;
use std::{
    ffi::c_void,
    sync::{LazyLock, RwLock},
};
use windows_sys::{
    Win32::Devices::HumanInterfaceDevice::DIDEVICEOBJECTDATA,
    Win32::Foundation::HINSTANCE,
    core::{GUID, HRESULT},
};

macro_rules! resolve_vtable_method {
    (direct_input, $this:expr, $field:ident) => {
        crate::runtime::direct_input_method($this, |context| context.$field)
    };
    (device, $this:expr, $field:ident) => {
        crate::runtime::device_method($this, |context| context.$field)
    };
}

macro_rules! observer_chain {
    (
        $dispatch_visibility:vis $dispatch:ident, $register:ident;
        target $method:ident;
        chain $chain:ident, context $context:ident;
        hook $hook:ident($($argument:ident: $argument_type:ty),* $(,)?);
        result $result:ty;
        resolve with $terminal:path;
    ) => {
        type $hook = unsafe fn($($argument_type),*);
        static $chain: LazyLock<RwLock<Vec<$hook>>> =
            LazyLock::new(|| RwLock::new(Vec::new()));
        #[allow(dead_code)]
        pub(crate) fn $register(hook: $hook) {
            let mut chain = $chain.write().unwrap_or_else(|error| error.into_inner());
            if !chain.iter().any(|registered| *registered as usize == hook as usize) {
                chain.push(hook);
            }
        }
        $dispatch_visibility unsafe fn $dispatch($($argument: $argument_type),*) -> $result {
            crate::register_hooks();
            let chain = $chain.read().unwrap_or_else(|error| error.into_inner());
            for hook in chain.iter().rev() {
                unsafe { hook($($argument),*) };
            }
            unsafe { $terminal($($argument),*) }
        }
    };
    (
        $dispatch_visibility:vis $dispatch:ident, $register:ident;
        target $method:ident;
        chain $chain:ident, context $context:ident;
        hook $hook:ident($this:ident: *mut c_void $(, $argument:ident: $argument_type:ty)* $(,)?);
        result $result:ty;
        resolve with $domain:ident, $field:ident -> $fallback:expr;
    ) => {
        type $hook = unsafe fn(*mut c_void $(, $argument_type)*);
        static $chain: LazyLock<RwLock<Vec<$hook>>> =
            LazyLock::new(|| RwLock::new(Vec::new()));
        #[allow(dead_code)]
        pub(crate) fn $register(hook: $hook) {
            let mut chain = $chain.write().unwrap_or_else(|error| error.into_inner());
            if !chain.iter().any(|registered| *registered as usize == hook as usize) {
                chain.push(hook);
            }
        }
        $dispatch_visibility unsafe fn $dispatch(
            $this: *mut c_void $(, $argument: $argument_type)*
        ) -> $result {
            crate::register_hooks();
            let chain = $chain.read().unwrap_or_else(|error| error.into_inner());
            for hook in chain.iter().rev() {
                unsafe { hook($this $(, $argument)*) };
            }
            let Some(method) = resolve_vtable_method!($domain, $this, $field) else {
                return $fallback;
            };
            let function = method.function;
            unsafe {
                crate::vtable::call_original($this, method, || {
                    function($this $(, $argument)*)
                })
            }
        }
    };
}

macro_rules! post_chain {
    (
        $dispatch_visibility:vis $dispatch:ident, $register:ident;
        target $method:ident;
        chain $chain:ident, context $context:ident;
        hook $hook:ident($($argument:ident: $argument_type:ty),* $(,)?);
        result $result:ty, $result_argument:ident;
        resolve with $resolver:ident: $resolver_type:ty;
    ) => {
        type $hook = unsafe fn($($argument_type,)* $result);
        static $chain: LazyLock<RwLock<Vec<$hook>>> =
            LazyLock::new(|| RwLock::new(Vec::new()));
        pub(crate) fn $register(hook: $hook) {
            let mut chain = $chain.write().unwrap_or_else(|error| error.into_inner());
            if !chain.iter().any(|registered| *registered as usize == hook as usize) {
                chain.push(hook);
            }
        }
        $dispatch_visibility unsafe fn $dispatch(
            $resolver: $resolver_type,
            $($argument: $argument_type),*
        ) -> $result {
            crate::register_hooks();
            let $result_argument = unsafe { $resolver($($argument),*) };
            let chain = $chain.read().unwrap_or_else(|error| error.into_inner());
            for hook in chain.iter().rev() {
                unsafe { hook($($argument,)* $result_argument) };
            }
            $result_argument
        }
    };
    (
        $dispatch_visibility:vis $dispatch:ident, $register:ident;
        target $method:ident;
        chain $chain:ident, context $context:ident;
        hook $hook:ident($this:ident: *mut c_void $(, $argument:ident: $argument_type:ty)* $(,)?);
        result $result:ty, $result_argument:ident;
        resolve with $domain:ident, $field:ident -> $fallback:expr;
    ) => {
        type $hook = unsafe fn(*mut c_void $(, $argument_type)*, $result);
        static $chain: LazyLock<RwLock<Vec<$hook>>> =
            LazyLock::new(|| RwLock::new(Vec::new()));
        pub(crate) fn $register(hook: $hook) {
            let mut chain = $chain.write().unwrap_or_else(|error| error.into_inner());
            if !chain.iter().any(|registered| *registered as usize == hook as usize) {
                chain.push(hook);
            }
        }
        $dispatch_visibility unsafe fn $dispatch(
            $this: *mut c_void $(, $argument: $argument_type)*
        ) -> $result {
            crate::register_hooks();
            let Some(method) = resolve_vtable_method!($domain, $this, $field) else {
                return $fallback;
            };
            let function = method.function;
            let $result_argument = unsafe {
                crate::vtable::call_original($this, method, || {
                    function($this $(, $argument)*)
                })
            };
            let chain = $chain.read().unwrap_or_else(|error| error.into_inner());
            for hook in chain.iter().rev() {
                unsafe { hook($this $(, $argument)*, $result_argument) };
            }
            $result_argument
        }
    };
}

macro_rules! around_chain {
    (
        $dispatch_visibility:vis $dispatch:ident, $register:ident;
        target $method:ident;
        chain $chain:ident, context $context:ident;
        hook $hook:ident($($argument:ident: $argument_type:ty),* $(,)?) -> $result:ty;
        resolve with $terminal:path;
    ) => {
        around_chain! {
            @define
            $dispatch_visibility $dispatch, $register;
            chain $chain, context $context;
            hook $hook($($argument: $argument_type),*) -> $result;
            resolver {};
            terminal $terminal;
        }
    };
    (
        $dispatch_visibility:vis $dispatch:ident, $register:ident;
        target $method:ident;
        chain $chain:ident, context $context:ident;
        hook $hook:ident($($argument:ident: $argument_type:ty),* $(,)?) -> $result:ty;
        resolve with $resolver:ident: $resolver_type:ty;
    ) => {
        around_chain! {
            @define
            $dispatch_visibility $dispatch, $register;
            chain $chain, context $context;
            hook $hook($($argument: $argument_type),*) -> $result;
            resolver { $resolver: $resolver_type };
            terminal |$($argument,)* context: $context<'_>| unsafe {
                (context.$resolver)($($argument),*)
            };
        }
    };
    (
        $dispatch_visibility:vis $dispatch:ident, $register:ident;
        target $method:ident;
        chain $chain:ident, context $context:ident;
        hook $hook:ident($this:ident: *mut c_void $(, $argument:ident: $argument_type:ty)* $(,)?) -> $result:ty;
        resolve with $domain:ident, $field:ident -> $fallback:expr;
    ) => {
        around_chain! {
            @define
            $dispatch_visibility $dispatch, $register;
            chain $chain, context $context;
            hook $hook($this: *mut c_void $(, $argument: $argument_type)*) -> $result;
            resolver {};
            terminal |$this, $($argument,)* _context: $context<'_>| unsafe {
                let Some(method) = resolve_vtable_method!($domain, $this, $field) else {
                    return $fallback;
                };
                let function = method.function;
                crate::vtable::call_original($this, method, || {
                    function($this $(, $argument)*)
                })
            };
        }
    };
    (
        @define
        $dispatch_visibility:vis $dispatch:ident, $register:ident;
        chain $chain:ident, context $context:ident;
        hook $hook:ident($($argument:ident: $argument_type:ty),* $(,)?) -> $result:ty;
        resolver { $($resolver:ident: $resolver_type:ty),* $(,)? };
        terminal $terminal:expr;
    ) => {
        pub(crate) type $hook = for<'a> unsafe fn(
            $($argument_type,)*
            $context<'a>,
        ) -> $result;

        pub(crate) struct $context<'a> {
            hooks: std::iter::Rev<std::slice::Iter<'a, $hook>>,
            terminal: &'a $hook,
            $($resolver: $resolver_type,)*
        }

        impl<'a> HookChain<'a, $hook> for $context<'a> {
            fn fp_next(&mut self) -> Option<&'a $hook> {
                Some(self.hooks.next().unwrap_or(self.terminal))
            }
        }

        static $chain: LazyLock<RwLock<Vec<$hook>>> =
            LazyLock::new(|| RwLock::new(Vec::new()));

        #[allow(dead_code)]
        pub(crate) fn $register(hook: $hook) {
            let mut chain = $chain.write().unwrap_or_else(|error| error.into_inner());
            if !chain.iter().any(|registered| *registered as usize == hook as usize) {
                chain.push(hook);
            }
        }

        $dispatch_visibility unsafe fn $dispatch(
            $($resolver: $resolver_type,)*
            $($argument: $argument_type),*
        ) -> $result {
            crate::register_hooks();
            let chain = $chain.read().unwrap_or_else(|error| error.into_inner());
            let terminal: $hook = $terminal;
            let mut hooks = chain.iter().rev();
            let next = hooks.next().unwrap_or(&terminal);
            unsafe {
                next(
                    $($argument,)*
                    $context { hooks, terminal: &terminal, $($resolver,)* },
                )
            }
        }
    };
}

pub(crate) use around_chain as __around_chain;
pub(crate) use observer_chain as __observer_chain;
pub(crate) use post_chain as __post_chain;

snowglass_hook::define_chain_macro! {
    pub(crate) chain => {
        observer: crate::hook::__observer_chain,
        post: crate::hook::__post_chain,
        around: crate::hook::__around_chain,
    },
    $
}

chain! {
    pub dispatch_direct_input_8_create, register_direct_input_8_create;
    after DirectInput8Create;
    chain DIRECT_INPUT_8_CREATE_CHAIN, context DirectInput8CreateContext;
    hook DirectInput8CreateHook(instance: HINSTANCE, version: u32, interface_id: *const GUID, output: *mut *mut c_void, outer: *mut c_void);
    result HRESULT, result;
    resolve with original: DirectInput8CreateFn;
}

chain! {
    pub(crate) dispatch_direct_input_release, register_direct_input_release;
    around DirectInputRelease;
    chain DIRECT_INPUT_RELEASE_CHAIN, context DirectInputReleaseContext;
    hook DirectInputReleaseHook(this: *mut c_void) -> u32;
    resolve with crate::runtime::direct_input_release;
}

chain! {
    pub(crate) dispatch_create_device, register_create_device;
    after CreateDevice;
    chain CREATE_DEVICE_CHAIN, context CreateDeviceContext;
    hook CreateDeviceHook(this: *mut c_void, class: *const GUID, output: *mut *mut c_void, outer: *mut c_void);
    result HRESULT, result;
    resolve with direct_input, original_create_device -> 0x8000_4005u32 as i32;
}

chain! {
    pub(crate) dispatch_device_release, register_device_release;
    around Release;
    chain DEVICE_RELEASE_CHAIN, context DeviceReleaseContext;
    hook DeviceReleaseHook(this: *mut c_void) -> u32;
    resolve with crate::runtime::device_release;
}

chain! {
    pub(crate) dispatch_acquire, register_acquire;
    around Acquire;
    chain ACQUIRE_CHAIN, context AcquireContext;
    hook AcquireHook(this: *mut c_void) -> HRESULT;
    resolve with device, original_acquire -> 0x8000_4005u32 as i32;
}

chain! {
    pub(crate) dispatch_get_device_state, register_get_device_state;
    around GetDeviceState;
    chain GET_DEVICE_STATE_CHAIN, context GetDeviceStateContext;
    hook GetDeviceStateHook(this: *mut c_void, size: u32, output: *mut c_void) -> HRESULT;
    resolve with device, original_get_device_state -> 0x8000_4005u32 as i32;
}

chain! {
    pub(crate) dispatch_set_data_format, register_set_data_format;
    around SetDataFormat;
    chain SET_DATA_FORMAT_CHAIN, context SetDataFormatContext;
    hook SetDataFormatHook(this: *mut c_void, format: *const c_void) -> HRESULT;
    resolve with device, original_set_data_format -> 0x8000_4005u32 as i32;
}

chain! {
    pub(crate) dispatch_get_device_data, register_get_device_data;
    around GetDeviceData;
    chain GET_DEVICE_DATA_CHAIN, context GetDeviceDataContext;
    hook GetDeviceDataHook(this: *mut c_void, object_size: u32, output: *mut DIDEVICEOBJECTDATA, count: *mut u32, flags: u32) -> HRESULT;
    resolve with device, original_get_device_data -> 0x8000_4005u32 as i32;
}

chain! {
    pub(crate) dispatch_set_cooperative_level, register_set_cooperative_level;
    around SetCooperativeLevel;
    chain SET_COOPERATIVE_LEVEL_CHAIN, context SetCooperativeLevelContext;
    hook SetCooperativeLevelHook(this: *mut c_void, window: *mut c_void, flags: u32) -> HRESULT;
    resolve with device, original_set_cooperative_level -> 0x8000_4005u32 as i32;
}

chain! {
    pub(crate) dispatch_set_event_notification, register_set_event_notification;
    observes SetEventNotification;
    chain SET_EVENT_NOTIFICATION_CHAIN, context SetEventNotificationContext;
    hook SetEventNotificationHook(this: *mut c_void, event: *mut c_void);
    result HRESULT;
    resolve with device, original_set_event_notification -> 0x8000_4005u32 as i32;
}
