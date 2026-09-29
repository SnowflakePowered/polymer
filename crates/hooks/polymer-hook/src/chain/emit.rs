#[doc(hidden)]
#[macro_export]
macro_rules! __chain_hooks_emit {
    (@emit $order:ident, $resolve:ident, [];
        [$($resolve_tokens:tt)*]
        [$($prefix:tt)*]
        [$($infix:tt)*]
        [$($postfix:tt)*]
        $visibility:vis unsafe extern $abi:literal fn $name:ident(
            $($argument:ident: $argument_type:ty),*
        ) -> $result:ty
        { no context; }
    ) => {
        $crate::__chain_hooks_emit!(@generate $order, $resolve;
            [$($resolve_tokens)*] [$($prefix)*] [$($infix)*] [$($postfix)*]
            $visibility unsafe extern $abi fn $name($($argument: $argument_type),*) -> $result;
            callback_context no;
            fields {}
        );
    };

    (@emit $order:ident, $resolve:ident, $common:tt;
        [$($resolve_tokens:tt)*] [$($prefix:tt)*] [$($infix:tt)*] [$($postfix:tt)*]
        $visibility:vis unsafe extern $abi:literal fn $name:ident(
            $($argument:ident: $argument_type:ty),*
        ) -> $result:ty { }
    ) => {
        $crate::__chain_hooks_emit!(@emit $order, $resolve, $common;
            [$($resolve_tokens)*] [$($prefix)*] [$($infix)*] [$($postfix)*]
            $visibility unsafe extern $abi fn $name($($argument: $argument_type),*) -> $result
            { no context; }
        );
    };

    (@emit $order:ident, $resolve:ident,
        [$shared:ident: $common_type:ty = $common_value:expr];
        [$($resolve_tokens:tt)*] [$($prefix:tt)*] [$($infix:tt)*] [$($postfix:tt)*]
        $visibility:vis unsafe extern $abi:literal fn $name:ident(
            $($argument:ident: $argument_type:ty),*
        ) -> $result:ty { no context; }
    ) => {
        $crate::__chain_hooks_emit!(@generate $order, $resolve;
            [$($resolve_tokens)*] [$($prefix)*] [$($infix)*] [$($postfix)*]
            $visibility unsafe extern $abi fn $name($($argument: $argument_type),*) -> $result;
            callback_context yes;
            fields { $shared: $common_type = $common_value }
        );
    };

    (@emit $order:ident, $resolve:ident, [];
        [$($resolve_tokens:tt)*]
        [$($prefix:tt)*]
        [$($infix:tt)*]
        [$($postfix:tt)*]
        $visibility:vis unsafe extern $abi:literal fn $name:ident(
            $($argument:ident: $argument_type:ty),*
        ) -> $result:ty
        { #[context] struct { $($field:ident: $field_type:ty = $field_value:expr),* $(,)? } }
    ) => {
        $crate::__chain_hooks_emit!(@generate $order, $resolve;
            [$($resolve_tokens)*] [$($prefix)*] [$($infix)*] [$($postfix)*]
            $visibility unsafe extern $abi fn $name($($argument: $argument_type),*) -> $result;
            callback_context yes;
            fields { $($field: $field_type = $field_value),* }
        );
    };

    (@emit $order:ident, $resolve:ident,
        [$shared:ident: $common_type:ty = $common_value:expr];
        [$($resolve_tokens:tt)*] [$($prefix:tt)*] [$($infix:tt)*] [$($postfix:tt)*]
        $visibility:vis unsafe extern $abi:literal fn $name:ident(
            $($argument:ident: $argument_type:ty),*
        ) -> $result:ty
        { #[context] struct { $($field:ident: $field_type:ty = $field_value:expr),* $(,)? } }
    ) => {
        $crate::__chain_hooks_emit!(@generate $order, $resolve;
            [$($resolve_tokens)*] [$($prefix)*] [$($infix)*] [$($postfix)*]
            $visibility unsafe extern $abi fn $name($($argument: $argument_type),*) -> $result;
            callback_context yes;
            fields {
                $shared: $common_type = $common_value,
                $($field: $field_type = $field_value),*
            }
        );
    };

    (@emit $order:ident, $resolve:ident, $common:tt;
        [$($resolve_tokens:tt)*]
        [$($prefix:tt)*]
        [$($infix:tt)*]
        [$($postfix:tt)*]
        $visibility:vis unsafe extern $abi:literal fn $name:ident(
            $($argument:ident: $argument_type:ty),*
        )
        $body:tt
    ) => {
        $crate::__chain_hooks_emit!(@emit $order, $resolve, $common;
            [$($resolve_tokens)*] [$($prefix)*] [$($infix)*] [$($postfix)*]
            $visibility unsafe extern $abi fn $name($($argument: $argument_type),*) -> ()
            $body
        );
    };

    (@generate $order:ident, $resolve:ident;
        [$($resolve_tokens:tt)*]
        [$($prefix:tt)*]
        [$($infix:tt)*]
        [$($postfix:tt)*]
        $visibility:vis unsafe extern $abi:literal fn $name:ident(
            $($argument:ident: $argument_type:ty),*
        ) -> $result:ty;
        callback_context $callback_context:ident;
        fields { $($field:ident: $field_type:ty = $field_value:expr),* }
    ) => { $crate::__paste! {
        $crate::__chain_hooks_terminal!(@declare_terminal
            [$($resolve_tokens)*]
            [$visibility]
            [[<$name:camel Terminal>]]
            [[<$name:camel TerminalFn>]]
            [[<$name:upper _TERMINAL>]]
            [[<install_ $name _terminal>]]
            [$name]
            [$($argument: $argument_type),*]
            [$result]
        );

        $visibility type [<$name:camel Hook>] = unsafe fn(
            $($argument_type,)*
            [<$name:camel Context>]<'_>,
        ) -> $result;

        $visibility struct [<$name:camel Context>]<'a> {
            hooks: &'a [[<$name:camel Hook>]],
            cursor: usize,
            reverse: bool,
            terminal: &'a [<$name:camel Hook>],
            $(#[allow(dead_code)] $visibility $field: &'a $field_type,)*
        }

        impl<'a> $crate::HookChain<'a, [<$name:camel Hook>]>
            for [<$name:camel Context>]<'a>
        {
            fn fp_next(&mut self) -> Option<&'a [<$name:camel Hook>]> {
                if self.reverse {
                    if self.cursor == 0 {
                        Some(self.terminal)
                    } else {
                        self.cursor -= 1;
                        Some(&self.hooks[self.cursor])
                    }
                } else if let Some(hook) = self.hooks.get(self.cursor) {
                    self.cursor += 1;
                    Some(hook)
                } else {
                    Some(self.terminal)
                }
            }
        }

        unsafe fn [<__chain_dispatch_ $name>](
            $($argument: $argument_type),*
        ) -> $result {
            unsafe fn chain_terminal(
                $($argument: $argument_type,)*
                _context: [<$name:camel Context>]<'_>,
            ) -> $result {
                $crate::__chain_hooks_terminal!(@call_terminal
                    [$resolve]
                    [$($resolve_tokens)*]
                    [[<$name:upper _TERMINAL>]]
                    [$($argument),*]
                )
            }

            $(let $field = $field_value;)*
            let terminal: [<$name:camel Hook>] = chain_terminal;
            let hooks: &[[<$name:camel Hook>]] =
                $crate::__chain_hooks_call!(@infix_slice $($infix)*);
            let reverse = $crate::__chain_hooks_call!(@is_reverse $order);
            let cursor = if reverse { hooks.len() } else { 0 };
            let context = [<$name:camel Context>] {
                hooks,
                cursor,
                reverse,
                terminal: &terminal,
                $($field: &$field,)*
            };

            let prefix_hooks: &[fn(
                $($argument_type,)*
                &[<$name:camel Context>]<'_>,
            )] = $crate::__chain_hooks_call!(@prefix_slice
                [$(($argument, $argument_type))*]
                [[<$name:camel Context>]]
                [$callback_context]
                [];
                $($prefix)*
            );
            if reverse {
                for hook in prefix_hooks.iter().rev() {
                    hook($($argument,)* &context);
                }
            } else {
                for hook in prefix_hooks {
                    hook($($argument,)* &context);
                }
            }

            let mut chain = context;
            let continuation = *$crate::HookChain::fp_next(&mut chain)
                .expect("hook chain has no continuation");
            let result = unsafe { continuation($($argument,)* chain) };

            let context = [<$name:camel Context>] {
                hooks: &[],
                cursor: 0,
                reverse: false,
                terminal: &terminal,
                $($field: &$field,)*
            };
            let postfix_hooks: &[fn(
                $($argument_type,)*
                $result,
                &[<$name:camel Context>]<'_>,
            )] = $crate::__chain_hooks_call!(@postfix_slice
                [$(($argument, $argument_type))*]
                [$result]
                [[<$name:camel Context>]]
                [$callback_context]
                [];
                $($postfix)*
            );
            if reverse {
                for hook in postfix_hooks.iter().rev() {
                    hook($($argument,)* result, &context);
                }
            } else {
                for hook in postfix_hooks {
                    hook($($argument,)* result, &context);
                }
            }
            result
        }

        $visibility unsafe extern $abi fn $name(
            $($argument: $argument_type),*
        ) -> $result {
            unsafe { [<__chain_dispatch_ $name>]($($argument),*) }
        }
    }};
}
