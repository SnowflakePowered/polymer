#[doc(hidden)]
#[macro_export]
macro_rules! __chain_hooks_terminal {
    (@declare_terminal
        [installed abi $terminal_abi:literal with fallback $fallback:expr;]
        [$visibility:vis]
        [$terminal_trait:ident]
        [$terminal_fn:ident]
        [$slot:ident]
        [$install:ident]
        [$hook:ident]
        [$($argument:ident: $argument_type:ty),*]
        [$result:ty]
    ) => {
        $visibility type $terminal_fn = unsafe extern $terminal_abi fn(
            $($argument_type),*
        ) -> $result;

        $visibility trait $terminal_trait: $crate::TerminalLifecycle {
            unsafe fn call(&self, $($argument: $argument_type),*) -> $result;
        }

        impl<T> $terminal_trait for T
        where
            T: $crate::TerminalCallable<Function = $terminal_fn>,
        {
            unsafe fn call(&self, $($argument: $argument_type),*) -> $result {
                let terminal = $crate::TerminalCallable::terminal_callable(self);
                unsafe { terminal($($argument),*) }
            }
        }

        static $slot: $crate::TerminalSlot<dyn $terminal_trait> =
            $crate::TerminalSlot::new();

        $visibility fn $install<T>(
            terminal: T,
        ) -> Result<
            $crate::TerminalInstallation<dyn $terminal_trait>,
            $crate::TerminalInstallError,
        >
        where
            T: $terminal_trait,
        {
            let terminal: ::std::sync::Arc<dyn $terminal_trait> =
                ::std::sync::Arc::new(terminal);
            $slot.install(terminal)
        }
    };
    (@declare_terminal
        [owned detour $detour:ty, target $target:expr,
            abi $terminal_abi:literal with fallback $fallback:expr;]
        [$visibility:vis]
        [$terminal_trait:ident]
        [$terminal_fn:ident]
        [$slot:ident]
        [$install:ident]
        [$hook:ident]
        [$($argument:ident: $argument_type:ty),*]
        [$result:ty]
    ) => {
        $crate::__chain_hooks_terminal!(@declare_owned_terminal
            [$detour] [$target] [] [$terminal_abi] [$visibility]
            [$terminal_trait] [$terminal_fn] [$slot] [$install] [$hook]
            [$($argument: $argument_type),*] [$result]
        );
    };
    (@declare_terminal
        [owned detour $detour:ty, target $target:expr, continuation $continuation:path,
            abi $terminal_abi:literal with fallback $fallback:expr;]
        [$visibility:vis]
        [$terminal_trait:ident]
        [$terminal_fn:ident]
        [$slot:ident]
        [$install:ident]
        [$hook:ident]
        [$($argument:ident: $argument_type:ty),*]
        [$result:ty]
    ) => {
        $crate::__chain_hooks_terminal!(@declare_owned_terminal
            [$detour] [$target] [$continuation] [$terminal_abi] [$visibility]
            [$terminal_trait] [$terminal_fn] [$slot] [$install] [$hook]
            [$($argument: $argument_type),*] [$result]
        );
    };
    (@declare_owned_terminal
        [$detour:ty] [$target:expr] [$($continuation:path)?] [$terminal_abi:literal]
        [$visibility:vis] [$terminal_trait:ident] [$terminal_fn:ident]
        [$slot:ident] [$install:ident] [$hook:ident]
        [$($argument:ident: $argument_type:ty),*] [$result:ty]
    ) => {
        $visibility type $terminal_fn = unsafe extern $terminal_abi fn(
            $($argument_type),*
        ) -> $result;

        $visibility trait $terminal_trait: $crate::TerminalLifecycle {
            unsafe fn call(&self, $($argument: $argument_type),*) -> $result;
        }

        impl<T> $terminal_trait for T
        where
            T: $crate::TerminalCallable<Function = $terminal_fn>,
        {
            unsafe fn call(&self, $($argument: $argument_type),*) -> $result {
                let terminal = $crate::TerminalCallable::terminal_callable(self);
                unsafe { terminal($($argument),*) }
            }
        }

        static $slot: $crate::TerminalSlot<dyn $terminal_trait> =
            $crate::TerminalSlot::new();

        $visibility fn $install() -> Result<
            $crate::TerminalInstallation<dyn $terminal_trait>,
            $crate::TerminalInstallError,
        > {
            let terminal = <$detour as $crate::TerminalDetour<$terminal_fn>>::create(
                $target as $terminal_fn,
                $hook as $terminal_fn,
            );
            $(let terminal = $crate::SelectedTerminal::new(terminal, $continuation);)?
            let terminal: ::std::sync::Arc<dyn $terminal_trait> =
                ::std::sync::Arc::new(terminal);
            $slot.install(terminal)
        }
    };
    (@declare_terminal
        [$($resolve_tokens:tt)*]
        [$visibility:vis]
        [$terminal_trait:ident]
        [$terminal_fn:ident]
        [$slot:ident]
        [$install:ident]
        [$hook:ident]
        [$($argument:ident: $argument_type:ty),*]
        [$result:ty]
    ) => {};

    (@call_terminal
        [$resolve:ident]
        [installed abi $terminal_abi:literal with fallback $fallback:expr;]
        [$slot:ident]
        [$($argument:ident),*]
    ) => {
        match $slot.acquire() {
            Some(terminal) => unsafe { terminal.terminal().call($($argument),*) },
            None => $fallback,
        }
    };
    (@call_terminal
        [$resolve:ident]
        [owned detour $detour:ty, target $target:expr,
            abi $terminal_abi:literal with fallback $fallback:expr;]
        [$slot:ident]
        [$($argument:ident),*]
    ) => {
        match $slot.acquire() {
            Some(terminal) => unsafe { terminal.terminal().call($($argument),*) },
            None => $fallback,
        }
    };
    (@call_terminal
        [$resolve:ident]
        [owned detour $detour:ty, target $target:expr, continuation $continuation:path,
            abi $terminal_abi:literal with fallback $fallback:expr;]
        [$slot:ident]
        [$($argument:ident),*]
    ) => {
        match $slot.acquire() {
            Some(terminal) => unsafe { terminal.terminal().call($($argument),*) },
            None => $fallback,
        }
    };
    (@call_terminal
        [$resolve:ident]
        [direct terminal $terminal:path;]
        [$slot:ident]
        [$($argument:ident),*]
    ) => {
        unsafe { $terminal($($argument),*) }
    };
    (@call_terminal
        [$resolve:ident]
        [$($resolve_tokens:tt)*]
        [$slot:ident]
        [$($argument:ident),*]
    ) => {
        ($resolve! {
            ($($argument),*);
            $($resolve_tokens)*
        })()
    };
}
