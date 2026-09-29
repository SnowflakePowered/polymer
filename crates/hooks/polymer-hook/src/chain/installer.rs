#[doc(hidden)]
#[macro_export]
macro_rules! __chain_hooks_installer {
    (@collect_detours [$order:ident] [$($choices:tt)*];
        #![cfg_attr($condition:meta, detour($backend:ty))] $($items:tt)*
    ) => {
        $crate::__chain_hooks_installer!(@collect_detours [$order] [$($choices)* [$condition] [$backend]]; $($items)*);
    };
    (@collect_detours [$order:ident] [$($choices:tt)*]; $($items:tt)*) => {
        $crate::__chain_hooks_installer!(@conditional_detours [$order] [$($items)*]; $($choices)*);
    };
    (@conditional_detours [$order:ident] $items:tt;) => {};
    (@conditional_detours [$order:ident] [$($items:tt)*];
        [$condition:meta] [$backend:ty] $($remaining:tt)*
    ) => {
        #[cfg($condition)]
        $crate::chain_hooks! {
            #![order($order)]
            #![detour($backend)]
            $($items)*
        }
        $crate::__chain_hooks_installer!(@conditional_detours [$order] [$($items)*]; $($remaining)*);
    };

    (@declare_installer
        [$visibility:vis] [$installer:ident] [stateless] [$($items:tt)*]
    ) => {
        $visibility struct $installer {
            terminals: ::std::vec::Vec<
                ::std::boxed::Box<dyn $crate::ErasedTerminalInstallation>
            >,
            complete: bool,
        }

        impl $installer {
            $visibility fn is_empty(&self) -> bool { self.terminals.is_empty() }
            $visibility fn is_complete(&self) -> bool { self.complete }

            fn create() -> Result<Self, $crate::HookInstallError<Self>> {
                let mut installation = Self {
                    terminals: ::std::vec::Vec::new(),
                    complete: false,
                };
                $crate::__chain_hooks_installer!(@install_terminals installation; $($items)*);
                installation.complete = true;
                Ok(installation)
            }

            fn uninstall_terminals(&mut self) -> Result<(), $crate::TerminalUninstallError> {
                let mut first_error = None;
                let mut index = self.terminals.len();
                while index != 0 {
                    index -= 1;
                    match self.terminals[index].uninstall() {
                        Ok(()) => { self.terminals.remove(index); }
                        Err(error) => { first_error.get_or_insert(error); }
                    }
                }
                first_error.map_or(Ok(()), Err)
            }
        }

        impl Drop for $installer {
            fn drop(&mut self) { let _ = self.uninstall_terminals(); }
        }
    };

    (@declare_installer
        [$visibility:vis] [$installer:ident] [state $state:ty] [$($items:tt)*]
    ) => { $crate::__paste! {
        static [<$installer:upper _STATE>]: $crate::__RwLock<
            Option<::std::sync::Arc<$state>>
        > = $crate::__RwLock::new(None);

        $visibility struct $installer {
            terminals: ::std::vec::Vec<
                ::std::boxed::Box<dyn $crate::ErasedTerminalInstallation>
            >,
            _state: ::std::sync::Arc<$state>,
            complete: bool,
        }

        impl $installer {
            $visibility fn is_empty(&self) -> bool { self.terminals.is_empty() }
            $visibility fn is_complete(&self) -> bool { self.complete }

            fn create() -> Result<Self, $crate::HookInstallError<Self>> {
                let state = ::std::sync::Arc::new(<$state as Default>::default());
                *[<$installer:upper _STATE>].write()
                    = Some(state.clone());
                let mut installation = Self {
                    terminals: ::std::vec::Vec::new(),
                    _state: state,
                    complete: false,
                };
                $crate::__chain_hooks_installer!(@install_terminals installation; $($items)*);
                installation.complete = true;
                Ok(installation)
            }

            fn uninstall_terminals(&mut self) -> Result<(), $crate::TerminalUninstallError> {
                let mut first_error = None;
                let mut index = self.terminals.len();
                while index != 0 {
                    index -= 1;
                    match self.terminals[index].uninstall() {
                        Ok(()) => { self.terminals.remove(index); }
                        Err(error) => { first_error.get_or_insert(error); }
                    }
                }
                if self.terminals.is_empty() {
                    *[<$installer:upper _STATE>].write()
                        = None;
                }
                first_error.map_or(Ok(()), Err)
            }
        }

        impl Drop for $installer {
            fn drop(&mut self) { let _ = self.uninstall_terminals(); }
        }
    }};

    (@install_terminals $installation:ident;) => {};
    (@install_terminals $installation:ident;
        #[terminal(installed($($terminal:tt)*), fallback = $fallback:expr $(,)?)]
        $(#[prefix($($prefix:tt)*)])?
        $(#[infix($($infix:path),* $(,)? )])?
        $(#[postfix($($postfix:tt)*)])?
        $visibility:vis unsafe extern $abi:literal fn $name:ident(
            $($argument:ident: $argument_type:ty),* $(,)?
        ) -> $result:ty;
        $($remaining:tt)*
    ) => { $crate::__paste! {
        match [<install_ $name _terminal>]() {
            Ok(terminal) => $installation.terminals.push(::std::boxed::Box::new(terminal)),
            Err(source) => {
                let _ = $installation.uninstall_terminals();
                return Err($crate::HookInstallError {
                    source,
                    installation: (!$installation.terminals.is_empty()).then_some($installation),
                });
            }
        }
        $crate::__chain_hooks_installer!(@install_terminals $installation; $($remaining)*);
    }};
    (@install_terminals $installation:ident;
        #[terminal(installed($($terminal:tt)*), fallback = $fallback:expr $(,)?)]
        $(#[prefix($($prefix:tt)*)])?
        $(#[infix($($infix:path),* $(,)? )])?
        $(#[postfix($($postfix:tt)*)])?
        $visibility:vis unsafe extern $abi:literal fn $name:ident(
            $($argument:ident: $argument_type:ty),* $(,)?
        );
        $($remaining:tt)*
    ) => { $crate::__paste! {
        match [<install_ $name _terminal>]() {
            Ok(terminal) => $installation.terminals.push(::std::boxed::Box::new(terminal)),
            Err(source) => {
                let _ = $installation.uninstall_terminals();
                return Err($crate::HookInstallError {
                    source,
                    installation: (!$installation.terminals.is_empty()).then_some($installation),
                });
            }
        }
        $crate::__chain_hooks_installer!(@install_terminals $installation; $($remaining)*);
    }};
    (@install_terminals $installation:ident; $token:tt $($remaining:tt)*) => {
        $crate::__chain_hooks_installer!(@install_terminals $installation; $($remaining)*);
    };
}
