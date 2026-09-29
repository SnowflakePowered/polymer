#[doc(hidden)]
#[macro_export]
macro_rules! __chain_hooks_items {
    (@items $order:ident, $resolve:ident, $detour:tt, $common:tt;) => {};

    (@items $order:ident, $resolve:ident, $detour:tt, $common:tt;
        #[trampoline]
        $($item:tt)*
    ) => {
        $crate::__chain_hooks_items!(@items $order, $resolve, $detour, $common; $($item)*);
    };

    (@items $order:ident, $resolve:ident, $detour:tt, $common:tt;
        #[trampoline(abi = $trampoline_abi:literal)]
        #[terminal($($terminal:tt)*)]
        $(#[prefix($($prefix:tt)*)])?
        $(#[infix($($infix:path),* $(,)? )])?
        $(#[postfix($($postfix:tt)*)])?
        $visibility:vis unsafe extern $abi:literal fn $name:ident(
            $($argument:ident: $argument_type:ty),* $(,)?
        ) $(-> $result:ty)?;
        $($remaining:tt)*
    ) => {
        $crate::__chain_hooks_items!(@items $order, $resolve, $detour, $common;
            #[terminal($($terminal)*)]
            $(#[prefix($($prefix)*)])?
            $(#[infix($($infix),*)])?
            $(#[postfix($($postfix)*)])?
            $visibility unsafe extern $trampoline_abi fn $name(
                $($argument: $argument_type),*
            ) $(-> $result)?;
            $($remaining)*
        );
    };

    (@items $order:ident, $resolve:ident, $detour:tt, $common:tt;
        #[trampoline(abi = $trampoline_abi:literal)]
        #[terminal($($terminal:tt)*)]
        $(#[prefix($($prefix:tt)*)])?
        $(#[infix($($infix:path),* $(,)? )])?
        $(#[postfix($($postfix:tt)*)])?
        $visibility:vis unsafe extern $abi:literal fn $name:ident(
            $($argument:ident: $argument_type:ty),* $(,)?
        ) $(-> $result:ty)? { $($body:tt)* }
        $($remaining:tt)*
    ) => {
        $crate::__chain_hooks_items!(@items $order, $resolve, $detour, $common;
            #[terminal($($terminal)*)]
            $(#[prefix($($prefix)*)])?
            $(#[infix($($infix),*)])?
            $(#[postfix($($postfix)*)])?
            $visibility unsafe extern $trampoline_abi fn $name(
                $($argument: $argument_type),*
            ) $(-> $result)? { $($body)* }
            $($remaining)*
        );
    };

    (@items $order:ident, $resolve:ident, $detour:tt, $common:tt;
        #[export($target:ident)]
        #[terminal($($terminal:tt)*)]
        $(#[prefix($($prefix:tt)*)])?
        $(#[infix($($infix:path),* $(,)? )])?
        $(#[postfix($($postfix:tt)*)])?
        $visibility:vis unsafe extern $abi:literal fn $name:ident(
            $($argument:ident: $argument_type:ty),* $(,)?
        ) $(-> $result:ty)?;
        $($remaining:tt)*
    ) => {
        $crate::__chain_hooks_items!(@items $order, $resolve, $detour, $common;
            #[terminal($($terminal)*)]
            $(#[prefix($($prefix)*)])?
            $(#[infix($($infix),*)])?
            $(#[postfix($($postfix)*)])?
            $visibility unsafe extern $abi fn $name(
                $($argument: $argument_type),*
            ) $(-> $result)?;
        );
        #[unsafe(no_mangle)]
        pub unsafe extern $abi fn $target(
            $($argument: $argument_type),*
        ) $(-> $result)? {
            unsafe { $name($($argument),*) }
        }
        $crate::__chain_hooks_items!(@items $order, $resolve, $detour, $common; $($remaining)*);
    };

    (@items $order:ident, $resolve:ident, $detour:tt, $common:tt;
        #[export($target:ident)]
        #[terminal($($terminal:tt)*)]
        $(#[prefix($($prefix:tt)*)])?
        $(#[infix($($infix:path),* $(,)? )])?
        $(#[postfix($($postfix:tt)*)])?
        $visibility:vis unsafe extern $abi:literal fn $name:ident(
            $($argument:ident: $argument_type:ty),* $(,)?
        ) $(-> $result:ty)? { $($body:tt)* }
        $($remaining:tt)*
    ) => {
        $crate::__chain_hooks_items!(@items $order, $resolve, $detour, $common;
            #[terminal($($terminal)*)]
            $(#[prefix($($prefix)*)])?
            $(#[infix($($infix),*)])?
            $(#[postfix($($postfix)*)])?
            $visibility unsafe extern $abi fn $name(
                $($argument: $argument_type),*
            ) $(-> $result)? { $($body)* }
        );
        #[unsafe(no_mangle)]
        pub unsafe extern $abi fn $target(
            $($argument: $argument_type),*
        ) $(-> $result)? {
            unsafe { $name($($argument),*) }
        }
        $crate::__chain_hooks_items!(@items $order, $resolve, $detour, $common; $($remaining)*);
    };

    // These two entry arms stay separate because a `ty` fragment cannot be
    // followed by an arbitrary token-tree fragment in `macro_rules!`.
    (@items $order:ident, $resolve:ident, $detour:tt, $common:tt;
        #[terminal($($terminal:tt)*)]
        $(#[prefix($($prefix:tt)*)])?
        $(#[infix($($infix:path),* $(,)? )])?
        $(#[postfix($($postfix:tt)*)])?
        $visibility:vis unsafe extern $abi:literal fn $name:ident(
            $($argument:ident: $argument_type:ty),* $(,)?
        ) $(-> $result:ty)?;
        $($remaining:tt)*
    ) => {
        $crate::__chain_hooks_items!(@normalize_terminal $order, $resolve, $abi, $detour, $common;
            [$($terminal)*]
            [$(prefix($($prefix)*))?]
            [$(infix($($infix),*))?]
            [$(postfix($($postfix)*))?]
            $visibility unsafe extern $abi fn $name(
                $($argument: $argument_type),*
            ) $(-> $result)? { no context; }
            then { $($remaining)* }
        );
    };

    (@items $order:ident, $resolve:ident, $detour:tt, $common:tt;
        #[terminal($($terminal:tt)*)]
        $(#[prefix($($prefix:tt)*)])?
        $(#[infix($($infix:path),* $(,)? )])?
        $(#[postfix($($postfix:tt)*)])?
        $visibility:vis unsafe extern $abi:literal fn $name:ident(
            $($argument:ident: $argument_type:ty),* $(,)?
        ) $(-> $result:ty)? { $($body:tt)* }
        $($remaining:tt)*
    ) => {
        $crate::__chain_hooks_items!(@normalize_terminal $order, $resolve, $abi, $detour, $common;
            [$($terminal)*]
            [$(prefix($($prefix)*))?]
            [$(infix($($infix),*))?]
            [$(postfix($($postfix)*))?]
            $visibility unsafe extern $abi fn $name(
                $($argument: $argument_type),*
            ) $(-> $result)? { $($body)* }
            then { $($remaining)* }
        );
    };

    (@normalize_terminal $order:ident, $resolve:ident, $terminal_abi:literal, $chain_detour:tt, $common:tt;
        [direct = $terminal:path]
        $($item:tt)*
    ) => {
        $crate::__chain_hooks_items!(@finish_item $order, $resolve, $chain_detour, $common;
            [direct terminal $terminal;]
            $($item)*
        );
    };

    (@normalize_terminal $order:ident, $resolve:ident, $terminal_abi:literal, $chain_detour:tt, $common:tt;
        [installed(detour = $detour:ty, target = $target:expr,
            continuation = $continuation:path $(,)?), fallback = $fallback:expr $(,)?]
        $($item:tt)*
    ) => {
        $crate::__chain_hooks_items!(@finish_item $order, $resolve, $chain_detour, $common;
            [owned detour $detour, target $target, continuation $continuation,
                abi $terminal_abi with fallback $fallback;]
            $($item)*
        );
    };

    (@normalize_terminal $order:ident, $resolve:ident, $terminal_abi:tt, [$detour:ty], $common:tt;
        [installed(target = $target:expr,
            continuation = $continuation:path $(,)?), fallback = $fallback:expr $(,)?]
        $($item:tt)*
    ) => {
        $crate::__chain_hooks_items!(@finish_item $order, $resolve, [$detour], $common;
            [owned detour $detour, target $target, continuation $continuation,
                abi $terminal_abi with fallback $fallback;]
            $($item)*
        );
    };

    (@normalize_terminal $order:ident, $resolve:ident, $terminal_abi:tt, [$detour:ty], $common:tt;
        [installed(target = $target:expr $(,)?), fallback = $fallback:expr $(,)?]
        $($item:tt)*
    ) => {
        $crate::__chain_hooks_items!(@finish_item $order, $resolve, [$detour], $common;
            [owned detour $detour, target $target,
                abi $terminal_abi with fallback $fallback;]
            $($item)*
        );
    };

    (@normalize_terminal $order:ident, $resolve:ident, $terminal_abi:literal, $chain_detour:tt, $common:tt;
        [installed(detour = $detour:ty, target = $target:expr $(,)?),
            fallback = $fallback:expr $(,)?]
        $($item:tt)*
    ) => {
        $crate::__chain_hooks_items!(@finish_item $order, $resolve, $chain_detour, $common;
            [owned detour $detour, target $target,
                abi $terminal_abi with fallback $fallback;]
            $($item)*
        );
    };

    (@normalize_terminal $order:ident, $resolve:ident, $terminal_abi:literal, $chain_detour:tt, $common:tt;
        [installed, fallback = $fallback:expr $(,)?]
        $($item:tt)*
    ) => {
        $crate::__chain_hooks_items!(@finish_item $order, $resolve, $chain_detour, $common;
            [installed abi $terminal_abi with fallback $fallback;]
            $($item)*
        );
    };

    (@normalize_terminal $order:ident, $resolve:ident, $terminal_abi:literal, $chain_detour:tt, $common:tt;
        [dynamic($resolver_argument:ident, $symbol:literal: $module:ident::$function_type:ident),
            fallback = $fallback:expr $(,)?]
        $($item:tt)*
    ) => {
        $crate::__chain_hooks_items!(@finish_item $order, $resolve, $chain_detour, $common;
            [dynamic proc $resolver_argument, $symbol: $module::$function_type;
                fallback $fallback;]
            $($item)*
        );
    };

    (@finish_item $order:ident, $resolve:ident, $detour:tt, $common:tt;
        [$($resolve_tokens:tt)*]
        [$($prefix:tt)*]
        [$($infix:tt)*]
        [$($postfix:tt)*]
        $visibility:vis unsafe extern $abi:literal fn $name:ident(
            $($argument:ident: $argument_type:ty),*
        ) $(-> $result:ty)? { $($body:tt)* }
        then { $($remaining:tt)* }
    ) => {
        $crate::__chain_hooks_emit! {
            @emit $order, $resolve, $common;
            [$($resolve_tokens)*]
            [$($prefix)*]
            [$($infix)*]
            [$($postfix)*]
            $visibility unsafe extern $abi fn $name(
                $($argument: $argument_type),*
            ) $(-> $result)? { $($body)* }
        }
        $crate::__chain_hooks_items!(@items $order, $resolve, $detour, $common; $($remaining)*);
    };
}
