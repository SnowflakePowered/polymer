//! Shared typed-hook infrastructure.

/// A continuation over a typed hook chain.
pub trait HookChain<'a, T: 'a> {
    /// Advances to the next registered hook, if one remains.
    fn fp_next(&mut self) -> Option<&'a T>;
}

/// Declares a backend-local `chain!` frontend which routes each chain mode to
/// its supplied backend expansion macro.
///
/// The dollar token is supplied by the caller because stable `macro_rules!`
/// cannot otherwise escape metavariables while defining another macro.
#[macro_export]
macro_rules! define_chain_macro {
    (
        $visibility:vis $name:ident => {
            observer: $observer:path,
            post: $post:path,
            around: $around:path $(,)?
        },
        $dollar:tt
    ) => {
        macro_rules! $name {
            (
                $dollar chain_visibility:vis $dollar dispatch:ident, $dollar register:ident;
                observe $dollar target:ident;
                $dollar($dollar tokens:tt)*
            ) => {
                $observer! {
                    $dollar chain_visibility $dollar dispatch, $dollar register;
                    target $dollar target;
                    $dollar($dollar tokens)*
                }
            };
            (
                $dollar chain_visibility:vis $dollar dispatch:ident, $dollar register:ident;
                observes $dollar target:ident;
                $dollar($dollar tokens:tt)*
            ) => {
                $observer! {
                    $dollar chain_visibility $dollar dispatch, $dollar register;
                    target $dollar target;
                    $dollar($dollar tokens)*
                }
            };
            (
                $dollar chain_visibility:vis $dollar dispatch:ident, $dollar register:ident;
                after $dollar target:ident;
                $dollar($dollar tokens:tt)*
            ) => {
                $post! {
                    $dollar chain_visibility $dollar dispatch, $dollar register;
                    target $dollar target;
                    $dollar($dollar tokens)*
                }
            };
            (
                $dollar chain_visibility:vis $dollar dispatch:ident, $dollar register:ident;
                around $dollar target:ident;
                $dollar($dollar tokens:tt)*
            ) => {
                $around! {
                    $dollar chain_visibility $dollar dispatch, $dollar register;
                    target $dollar target;
                    $dollar($dollar tokens)*
                }
            };
        }
        $visibility use $name;
    };
}
