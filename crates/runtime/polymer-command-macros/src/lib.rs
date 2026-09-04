//! Procedural macros used by `polymer-command` handlers.

#![forbid(unsafe_code)]

use proc_macro::TokenStream;
use quote::quote;
use syn::{
    FnArg, GenericParam, ImplItem, ImplItemFn, ItemImpl, Lifetime, LifetimeParam, ReturnType, Type,
    parse_macro_input,
};

/// Adapts naturally-returning methods to generated command handler methods.
///
/// Methods returning `Self::Output` keep their explicit dispatch control flow.
/// Every other method returns its response normally; the generated adapter sends
/// that value to the caller and produces the default dispatch output. Async
/// methods are boxed into the handler's `Self::Output` future.
#[proc_macro_attribute]
pub fn command_handler(arguments: TokenStream, item: TokenStream) -> TokenStream {
    if !arguments.is_empty() {
        return compile_error("command_handler does not accept arguments");
    }
    let mut implementation = parse_macro_input!(item as ItemImpl);
    for item in &mut implementation.items {
        let ImplItem::Fn(method) = item else {
            continue;
        };
        if !returns_dispatch_output(method)
            && let Err(error) = rewrite(method)
        {
            return error.into_compile_error().into();
        }
    }
    quote!(#implementation).into()
}

fn returns_dispatch_output(method: &ImplItemFn) -> bool {
    let ReturnType::Type(_, return_type) = &method.sig.output else {
        return false;
    };
    let Type::Path(return_type) = return_type.as_ref() else {
        return false;
    };
    let mut segments = return_type.path.segments.iter();
    matches!(
        (segments.next(), segments.next(), segments.next()),
        (Some(first), Some(second), None)
            if first.ident == "Self" && second.ident == "Output"
    )
}

fn rewrite(method: &mut ImplItemFn) -> syn::Result<()> {
    let asynchronous = method.sig.asyncness.take().is_some();
    let ReturnType::Type(_, response_type) = &method.sig.output else {
        return Err(syn::Error::new_spanned(
            &method.sig,
            "command handlers must declare their response type",
        ));
    };
    let response_type = response_type.clone();
    let lifetime: Lifetime = syn::parse_quote!('command_handler);
    method.sig.generics.params.insert(
        0,
        GenericParam::Lifetime(LifetimeParam::new(lifetime.clone())),
    );

    let Some(FnArg::Receiver(receiver)) = method.sig.inputs.first_mut() else {
        return Err(syn::Error::new_spanned(
            &method.sig,
            "command handlers require &mut self",
        ));
    };
    let Some((_, receiver_lifetime)) = &mut receiver.reference else {
        return Err(syn::Error::new_spanned(
            receiver,
            "command handlers require &mut self",
        ));
    };
    if receiver.mutability.is_none() {
        return Err(syn::Error::new_spanned(
            receiver,
            "command handlers require &mut self",
        ));
    }
    *receiver_lifetime = Some(lifetime.clone());
    method.sig.inputs.push(syn::parse_quote!(
        response: ::polymer_command::CommandResponse<#response_type>
    ));
    method.sig.output = syn::parse_quote!(-> Self::Output<#lifetime>);

    let body = method.block.clone();
    method.block = if asynchronous {
        syn::parse_quote!({
            ::std::boxed::Box::pin(async move {
                let command_response: #response_type = (async move #body).await;
                let _ = response.send(command_response);
                ::core::default::Default::default()
            })
        })
    } else {
        syn::parse_quote!({
            let command_response = (|| -> #response_type #body)();
            let _ = response.send(command_response);
            ::core::default::Default::default()
        })
    };
    Ok(())
}

fn compile_error(message: &str) -> TokenStream {
    syn::Error::new(proc_macro2::Span::call_site(), message)
        .into_compile_error()
        .into()
}
