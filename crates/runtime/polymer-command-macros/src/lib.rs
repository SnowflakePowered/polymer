//! Protocol handler generation for native actors and thread-owned executors.
#![forbid(unsafe_code)]
mod local;
mod native;
use proc_macro::TokenStream;
use syn::{ItemImpl, parse_macro_input};

/// Generates native Kameo message handlers, with optional context delegation.
#[proc_macro_attribute]
pub fn native_handler(arguments: TokenStream, item: TokenStream) -> TokenStream {
    let protocol = parse_macro_input!(arguments as syn::Path);
    let implementation = parse_macro_input!(item as ItemImpl);
    native::NativeHandler::expand(protocol, implementation)
        .unwrap_or_else(syn::Error::into_compile_error)
        .into()
}

/// Completes a Kameo reply after a naturally-returning local handler finishes.
/// Methods returning Self::Output retain explicit local scheduling control.
#[proc_macro_attribute]
pub fn command_handler(arguments: TokenStream, item: TokenStream) -> TokenStream {
    if !arguments.is_empty() {
        return syn::Error::new(
            proc_macro2::Span::call_site(),
            "command_handler does not accept arguments",
        )
        .into_compile_error()
        .into();
    }
    local::LocalHandler::expand(parse_macro_input!(item as ItemImpl))
        .unwrap_or_else(syn::Error::into_compile_error)
        .into()
}
