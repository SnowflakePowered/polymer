//! Typed hook chains with argument projection.
//!
//! An exported Vulkan command can carry more ABI arguments than an observer
//! needs. The chain owns continuation through every observer; handlers only
//! receive the arguments declared when they are registered.

pub(crate) trait HookChain<'a, T: 'a> {
    fn fp_next(&mut self) -> Option<&'a T>;
}

macro_rules! resolve_proc {
    (command_buffer, $handle:expr, $name:expr, $ty:ty) => {{
        crate::layer::command_dispatch_data($handle).and_then(|(gdpa, device)| unsafe {
            crate::terminal::device_proc::<$ty>(gdpa, device, $name)
        })
    }};
    (device, $handle:expr, $name:expr, $ty:ty) => {{
        crate::layer::device_gdpa($handle)
            .and_then(|gdpa| unsafe { crate::terminal::device_proc::<$ty>(gdpa, $handle, $name) })
    }};
    (instance, $handle:expr, $name:expr, $ty:ty) => {{
        crate::layer::instance_gipa($handle)
            .and_then(|gipa| unsafe { crate::terminal::instance_proc::<$ty>(gipa, $handle, $name) })
    }};
    (queue, $handle:expr, $name:expr, $ty:ty) => {{
        crate::layer::queue_dispatch_data($handle).and_then(|(gdpa, device)| unsafe {
            crate::terminal::device_proc::<$ty>(gdpa, device, $name)
        })
    }};
}
pub(crate) use resolve_proc;

macro_rules! observer_chain {
    (
        $vis:vis $dispatch:ident, $register:ident;
        export $export:ident;
        chain $chain:ident, context $context:ident;
        hook $hook_ty:ident($($hook_arg:ident: $hook_arg_ty:ty),* $(,)?);
        abi ($($abi_arg:ident: $abi_arg_ty:ty),* $(,)?);
        resolve with $resolve_arg:ident, $name:literal: $next_module:ident::$next_ty:ident;
    ) => {
        type $hook_ty = fn($($hook_arg_ty),*);
        static $chain: std::sync::LazyLock<std::sync::RwLock<Vec<$hook_ty>>> =
            std::sync::LazyLock::new(|| std::sync::RwLock::new(Vec::new()));
        struct $context<'a> { hooks: std::slice::Iter<'a, $hook_ty> }
        impl<'a> HookChain<'a, $hook_ty> for $context<'a> {
            fn fp_next(&mut self) -> Option<&'a $hook_ty> { self.hooks.next() }
        }
        $vis fn $register(hook: $hook_ty) {
            let mut chain = $chain.write().unwrap_or_else(|error| error.into_inner());
            if !chain.iter().any(|registered| *registered as usize == hook as usize) { chain.push(hook); }
        }
        $vis unsafe fn $dispatch($($abi_arg: $abi_arg_ty),*) {
            let chain = $chain.read().unwrap_or_else(|error| error.into_inner());
            let mut context = $context { hooks: chain.iter() };
            while let Some(next) = context.fp_next() { next($($hook_arg),*); }
            let resolved: Option<$next_module::$next_ty> = $crate::hook_chain::resolve_proc!($resolve_arg, $resolve_arg, $name, $next_module::$next_ty);
            if let Some(next) = resolved { unsafe { next($($abi_arg),*) }; }
        }
        #[unsafe(no_mangle)]
        pub unsafe extern "system" fn $export($($abi_arg: $abi_arg_ty),*) {
            crate::register_hooks(); unsafe { $dispatch($($abi_arg),*) };
        }
    };
    (
        $vis:vis $dispatch:ident, $register:ident;
        export $export:ident;
        chain $chain:ident, context $context:ident;
        hook $hook_ty:ident($($hook_arg:ident: $hook_arg_ty:ty),* $(,)?);
        abi ($($abi_arg:ident: $abi_arg_ty:ty),* $(,)?);
        resolve with $terminal:path;
    ) => {
        type $hook_ty = fn($($hook_arg_ty),*);
        static $chain: std::sync::LazyLock<std::sync::RwLock<Vec<$hook_ty>>> =
            std::sync::LazyLock::new(|| std::sync::RwLock::new(Vec::new()));

        struct $context<'a> {
            hooks: std::slice::Iter<'a, $hook_ty>,
        }

        impl<'a> HookChain<'a, $hook_ty> for $context<'a> {
            fn fp_next(&mut self) -> Option<&'a $hook_ty> {
                self.hooks.next()
            }
        }

        $vis fn $register(hook: $hook_ty) {
            let mut chain = $chain.write().unwrap_or_else(|error| error.into_inner());
            if !chain.iter().any(|registered| *registered as usize == hook as usize) {
                chain.push(hook);
            }
        }

        $vis unsafe fn $dispatch($($abi_arg: $abi_arg_ty),*) {
            let chain = $chain.read().unwrap_or_else(|error| error.into_inner());
            let mut context = $context { hooks: chain.iter() };
            while let Some(next) = context.fp_next() {
                next($($hook_arg),*);
            }
            unsafe { $terminal($($abi_arg),*) };
        }

        #[unsafe(no_mangle)]
        pub unsafe extern "system" fn $export($($abi_arg: $abi_arg_ty),*) {
            crate::register_hooks();
            unsafe { $dispatch($($abi_arg),*) };
        }
    };

    (
        $vis:vis $dispatch:ident, $register:ident;
        export $export:ident;
        chain $chain:ident, context $context:ident;
        hook $hook_ty:ident($($hook_arg:ident: $hook_arg_ty:ty),* $(,)?);
        abi ($($abi_arg:ident: $abi_arg_ty:ty),* $(,)?);
        result $result:ty;
        resolve with $resolve_arg:ident, $name:literal: $next_module:ident::$next_ty:ident -> $fallback:expr;
    ) => {
        type $hook_ty = fn($($hook_arg_ty),*);
        static $chain: std::sync::LazyLock<std::sync::RwLock<Vec<$hook_ty>>> =
            std::sync::LazyLock::new(|| std::sync::RwLock::new(Vec::new()));
        struct $context<'a> { hooks: std::slice::Iter<'a, $hook_ty> }
        impl<'a> HookChain<'a, $hook_ty> for $context<'a> { fn fp_next(&mut self) -> Option<&'a $hook_ty> { self.hooks.next() } }
        $vis fn $register(hook: $hook_ty) {
            let mut chain = $chain.write().unwrap_or_else(|error| error.into_inner());
            if !chain.iter().any(|registered| *registered as usize == hook as usize) { chain.push(hook); }
        }
        $vis unsafe fn $dispatch($($abi_arg: $abi_arg_ty),*) -> $result {
            let chain = $chain.read().unwrap_or_else(|error| error.into_inner());
            let mut context = $context { hooks: chain.iter() };
            while let Some(next) = context.fp_next() { next($($hook_arg),*); }
            let resolved: Option<$next_module::$next_ty> = $crate::hook_chain::resolve_proc!($resolve_arg, $resolve_arg, $name, $next_module::$next_ty);
            resolved.map_or($fallback, |next| unsafe { next($($abi_arg),*) })
        }
        #[unsafe(no_mangle)]
        pub unsafe extern "system" fn $export($($abi_arg: $abi_arg_ty),*) -> $result {
            crate::register_hooks(); unsafe { $dispatch($($abi_arg),*) }
        }
    };
    (
        $vis:vis $dispatch:ident, $register:ident;
        export $export:ident;
        chain $chain:ident, context $context:ident;
        hook $hook_ty:ident($($hook_arg:ident: $hook_arg_ty:ty),* $(,)?);
        abi ($($abi_arg:ident: $abi_arg_ty:ty),* $(,)?);
        result $result:ty;
        resolve with $terminal:path;
    ) => {
        type $hook_ty = fn($($hook_arg_ty),*);
        static $chain: std::sync::LazyLock<std::sync::RwLock<Vec<$hook_ty>>> =
            std::sync::LazyLock::new(|| std::sync::RwLock::new(Vec::new()));
        struct $context<'a> { hooks: std::slice::Iter<'a, $hook_ty> }
        impl<'a> HookChain<'a, $hook_ty> for $context<'a> {
            fn fp_next(&mut self) -> Option<&'a $hook_ty> { self.hooks.next() }
        }
        $vis fn $register(hook: $hook_ty) {
            let mut chain = $chain.write().unwrap_or_else(|error| error.into_inner());
            if !chain.iter().any(|registered| *registered as usize == hook as usize) { chain.push(hook); }
        }
        $vis unsafe fn $dispatch($($abi_arg: $abi_arg_ty),*) -> $result {
            let chain = $chain.read().unwrap_or_else(|error| error.into_inner());
            let mut context = $context { hooks: chain.iter() };
            while let Some(next) = context.fp_next() { next($($hook_arg),*); }
            unsafe { $terminal($($abi_arg),*) }
        }
        #[unsafe(no_mangle)]
        pub unsafe extern "system" fn $export($($abi_arg: $abi_arg_ty),*) -> $result {
            crate::register_hooks();
            unsafe { $dispatch($($abi_arg),*) }
        }
    };
}

pub(crate) use observer_chain as __observer_chain;

macro_rules! post_chain {
    (
        $vis:vis $dispatch:ident, $register:ident;
        export $export:ident;
        chain $chain:ident, context $context:ident;
        hook $hook_ty:ident($($hook_arg:ident: $hook_arg_ty:ty),* $(,)?);
        abi ($($abi_arg:ident: $abi_arg_ty:ty),* $(,)?);
        resolve with $resolve_arg:ident, $name:literal: $next_module:ident::$next_ty:ident;
    ) => {
        type $hook_ty = fn($($hook_arg_ty),*);
        static $chain: std::sync::LazyLock<std::sync::RwLock<Vec<$hook_ty>>> = std::sync::LazyLock::new(|| std::sync::RwLock::new(Vec::new()));
        struct $context<'a> { hooks: std::slice::Iter<'a, $hook_ty> }
        impl<'a> HookChain<'a, $hook_ty> for $context<'a> { fn fp_next(&mut self) -> Option<&'a $hook_ty> { self.hooks.next() } }
        $vis fn $register(hook: $hook_ty) {
            let mut chain = $chain.write().unwrap_or_else(|error| error.into_inner());
            if !chain.iter().any(|registered| *registered as usize == hook as usize) { chain.push(hook); }
        }
        $vis unsafe fn $dispatch($($abi_arg: $abi_arg_ty),*) {
            let resolved: Option<$next_module::$next_ty> = $crate::hook_chain::resolve_proc!($resolve_arg, $resolve_arg, $name, $next_module::$next_ty);
            if let Some(next) = resolved {
                unsafe { next($($abi_arg),*) };
            }
            let chain = $chain.read().unwrap_or_else(|error| error.into_inner());
            let mut context = $context { hooks: chain.iter() };
            while let Some(next) = context.fp_next() { next($($hook_arg),*); }
        }
        #[unsafe(no_mangle)]
        pub unsafe extern "system" fn $export($($abi_arg: $abi_arg_ty),*) {
            crate::register_hooks(); unsafe { $dispatch($($abi_arg),*) };
        }
    };
    (
        $vis:vis $dispatch:ident, $register:ident;
        export $export:ident;
        chain $chain:ident, context $context:ident;
        hook $hook_ty:ident($($hook_arg:ident: $hook_arg_ty:ty),* $(,)?);
        abi ($($abi_arg:ident: $abi_arg_ty:ty),* $(,)?);
        resolve with $terminal:path;
    ) => {
        type $hook_ty = fn($($hook_arg_ty),*);
        static $chain: std::sync::LazyLock<std::sync::RwLock<Vec<$hook_ty>>> = std::sync::LazyLock::new(|| std::sync::RwLock::new(Vec::new()));
        struct $context<'a> { hooks: std::slice::Iter<'a, $hook_ty> }
        impl<'a> HookChain<'a, $hook_ty> for $context<'a> { fn fp_next(&mut self) -> Option<&'a $hook_ty> { self.hooks.next() } }
        $vis fn $register(hook: $hook_ty) {
            let mut chain = $chain.write().unwrap_or_else(|error| error.into_inner());
            if !chain.iter().any(|registered| *registered as usize == hook as usize) { chain.push(hook); }
        }
        $vis unsafe fn $dispatch($($abi_arg: $abi_arg_ty),*) {
            unsafe { $terminal($($abi_arg),*) };
            let chain = $chain.read().unwrap_or_else(|error| error.into_inner());
            let mut context = $context { hooks: chain.iter() };
            while let Some(next) = context.fp_next() { next($($hook_arg),*); }
        }
        #[unsafe(no_mangle)]
        pub unsafe extern "system" fn $export($($abi_arg: $abi_arg_ty),*) {
            crate::register_hooks(); unsafe { $dispatch($($abi_arg),*) };
        }
    };
    (
        $vis:vis $dispatch:ident, $register:ident;
        export $export:ident;
        chain $chain:ident, context $context:ident;
        hook $hook_ty:ident($($hook_arg:ident: $hook_arg_ty:ty),* $(,)?);
        abi ($($abi_arg:ident: $abi_arg_ty:ty),* $(,)?);
        result $result:ty, $result_arg:ident;
        resolve with $resolve_arg:ident, $name:literal: $next_module:ident::$next_ty:ident -> $fallback:expr;
    ) => {
        type $hook_ty = fn($($hook_arg_ty,)* $result);
        static $chain: std::sync::LazyLock<std::sync::RwLock<Vec<$hook_ty>>> =
            std::sync::LazyLock::new(|| std::sync::RwLock::new(Vec::new()));
        struct $context<'a> { hooks: std::slice::Iter<'a, $hook_ty> }
        impl<'a> HookChain<'a, $hook_ty> for $context<'a> { fn fp_next(&mut self) -> Option<&'a $hook_ty> { self.hooks.next() } }
        $vis fn $register(hook: $hook_ty) {
            let mut chain = $chain.write().unwrap_or_else(|error| error.into_inner());
            if !chain.iter().any(|registered| *registered as usize == hook as usize) { chain.push(hook); }
        }
        $vis unsafe fn $dispatch($($abi_arg: $abi_arg_ty),*) -> $result {
            let resolved: Option<$next_module::$next_ty> = $crate::hook_chain::resolve_proc!($resolve_arg, $resolve_arg, $name, $next_module::$next_ty);
            let $result_arg = resolved.map_or($fallback, |next| unsafe { next($($abi_arg),*) });
            let chain = $chain.read().unwrap_or_else(|error| error.into_inner());
            let mut context = $context { hooks: chain.iter() };
            while let Some(next) = context.fp_next() { next($($hook_arg,)* $result_arg); }
            $result_arg
        }
        #[unsafe(no_mangle)]
        pub unsafe extern "system" fn $export($($abi_arg: $abi_arg_ty),*) -> $result {
            crate::register_hooks(); unsafe { $dispatch($($abi_arg),*) }
        }
    };
    (
        $vis:vis $dispatch:ident, $register:ident;
        export $export:ident;
        chain $chain:ident, context $context:ident;
        hook $hook_ty:ident($($hook_arg:ident: $hook_arg_ty:ty),* $(,)?);
        abi ($($abi_arg:ident: $abi_arg_ty:ty),* $(,)?);
        result $result:ty, $result_arg:ident;
        resolve with $terminal:path;
    ) => {
        type $hook_ty = fn($($hook_arg_ty,)* $result);
        static $chain: std::sync::LazyLock<std::sync::RwLock<Vec<$hook_ty>>> =
            std::sync::LazyLock::new(|| std::sync::RwLock::new(Vec::new()));
        struct $context<'a> { hooks: std::slice::Iter<'a, $hook_ty> }
        impl<'a> HookChain<'a, $hook_ty> for $context<'a> { fn fp_next(&mut self) -> Option<&'a $hook_ty> { self.hooks.next() } }
        $vis fn $register(hook: $hook_ty) {
            let mut chain = $chain.write().unwrap_or_else(|error| error.into_inner());
            if !chain.iter().any(|registered| *registered as usize == hook as usize) { chain.push(hook); }
        }
        $vis unsafe fn $dispatch($($abi_arg: $abi_arg_ty),*) -> $result {
            let $result_arg = unsafe { $terminal($($abi_arg),*) };
            let chain = $chain.read().unwrap_or_else(|error| error.into_inner());
            let mut context = $context { hooks: chain.iter() };
            while let Some(next) = context.fp_next() { next($($hook_arg,)* $result_arg); }
            $result_arg
        }
        #[unsafe(no_mangle)]
        pub unsafe extern "system" fn $export($($abi_arg: $abi_arg_ty),*) -> $result {
            crate::register_hooks(); unsafe { $dispatch($($abi_arg),*) }
        }
    };
}
pub(crate) use post_chain as __post_chain;

macro_rules! around_chain {
    (
        $vis:vis $dispatch:ident, $register:ident;
        export $export:ident;
        chain $chain:ident, context $context:ident;
        hook $hook_ty:ident($($abi_arg:ident: $abi_arg_ty:ty),* $(,)?; next: $next_ty:ty);
        result $result:ty;
        resolve with $resolve_arg:ident, $name:literal: $next_module:ident::$resolved_ty:ident -> $fallback:expr;
    ) => {
        type $hook_ty = unsafe fn($($abi_arg_ty,)* $next_ty) -> $result;
        static $chain: std::sync::LazyLock<std::sync::RwLock<Vec<$hook_ty>>> = std::sync::LazyLock::new(|| std::sync::RwLock::new(Vec::new()));
        struct $context<'a> { hooks: std::slice::Iter<'a, $hook_ty> }
        impl<'a> HookChain<'a, $hook_ty> for $context<'a> { fn fp_next(&mut self) -> Option<&'a $hook_ty> { self.hooks.next() } }
        $vis fn $register(hook: $hook_ty) { $chain.write().unwrap_or_else(|e| e.into_inner()).push(hook); }
        $vis unsafe fn $dispatch($($abi_arg: $abi_arg_ty),*) -> $result {
            let Some(next): Option<$next_module::$resolved_ty> = $crate::hook_chain::resolve_proc!($resolve_arg, $resolve_arg, $name, $next_module::$resolved_ty)
            else { return $fallback; };
            let chain = $chain.read().unwrap_or_else(|e| e.into_inner());
            let mut context = $context { hooks: chain.iter() };
            if let Some(handler) = context.fp_next() { unsafe { handler($($abi_arg,)* next) } }
            else { unsafe { next($($abi_arg),*) } }
        }
        #[unsafe(no_mangle)]
        pub unsafe extern "system" fn $export($($abi_arg: $abi_arg_ty),*) -> $result {
            crate::register_hooks(); unsafe { $dispatch($($abi_arg),*) }
        }
    };
}
pub(crate) use around_chain as __around_chain;

macro_rules! chain {
    ($vis:vis $dispatch:ident, $register:ident; observe $export:ident; $($tokens:tt)*) => {
        $crate::hook_chain::__observer_chain! {
            $vis $dispatch, $register;
            export $export;
            $($tokens)*
        }
    };
    ($vis:vis $dispatch:ident, $register:ident; observes $export:ident; $($tokens:tt)*) => {
        $crate::hook_chain::__observer_chain! {
            $vis $dispatch, $register;
            export $export;
            $($tokens)*
        }
    };
    ($vis:vis $dispatch:ident, $register:ident; after $export:ident; $($tokens:tt)*) => {
        $crate::hook_chain::__post_chain! {
            $vis $dispatch, $register;
            export $export;
            $($tokens)*
        }
    };
    ($vis:vis $dispatch:ident, $register:ident; around $export:ident; $($tokens:tt)*) => {
        $crate::hook_chain::__around_chain! {
            $vis $dispatch, $register;
            export $export;
            $($tokens)*
        }
    };
}
pub(crate) use chain;
