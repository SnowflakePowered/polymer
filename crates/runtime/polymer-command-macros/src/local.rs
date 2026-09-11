use quote::quote;
use syn::{
    FnArg, GenericParam, ImplItem, ImplItemFn, ItemImpl, Lifetime, LifetimeParam, ReturnType, Type,
};
pub(crate) struct LocalHandler;
impl LocalHandler {
    pub(crate) fn expand(mut implementation: ItemImpl) -> syn::Result<proc_macro2::TokenStream> {
        for item in &mut implementation.items {
            let ImplItem::Fn(method) = item else {
                continue;
            };
            if !Self::returns_dispatch_output(method) {
                Self::rewrite(method)?;
            }
        }
        Ok(quote!(#implementation))
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
            response: ::polymer_command::Completion<#response_type>
        ));
        method.sig.output = syn::parse_quote!(-> Self::Output<#lifetime>);

        let body = method.block.clone();
        method.block = if asynchronous {
            syn::parse_quote!({
                ::std::boxed::Box::pin(async move {
                    let command_response: #response_type = (async move #body).await;
                    response.send(command_response);
                    ::core::default::Default::default()
                })
            })
        } else {
            syn::parse_quote!({
                let command_response = (|| -> #response_type #body)();
                response.send(command_response);
                ::core::default::Default::default()
            })
        };
        Ok(())
    }
}
