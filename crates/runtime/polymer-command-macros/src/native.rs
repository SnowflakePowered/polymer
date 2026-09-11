use proc_macro2::TokenStream;
use quote::{format_ident, quote};
use syn::{FnArg, ImplItem, ItemImpl, Pat, Path, ReturnType};

pub(crate) struct NativeHandler;

impl NativeHandler {
    pub(crate) fn expand(protocol: Path, implementation: ItemImpl) -> syn::Result<TokenStream> {
        let actor = &implementation.self_ty;
        let mut handlers = TokenStream::new();
        for item in &implementation.items {
            let ImplItem::Fn(method) = item else {
                return Err(syn::Error::new_spanned(
                    item,
                    "native handlers contain only methods",
                ));
            };
            let ReturnType::Type(_, reply) = &method.sig.output else {
                return Err(syn::Error::new_spanned(
                    method,
                    "declare the native reply type",
                ));
            };
            let mut message = protocol.clone();
            let segment = message.segments.last_mut().expect("protocol path");
            let suffix: String = method
                .sig
                .ident
                .to_string()
                .split('_')
                .map(|part| {
                    let mut chars = part.chars();
                    chars
                        .next()
                        .map(|first| first.to_uppercase().to_string() + chars.as_str())
                        .unwrap_or_default()
                })
                .collect();
            segment.ident = format_ident!("{}{}", segment.ident, suffix);
            let mut arguments = Vec::new();
            for argument in method.sig.inputs.iter().skip(1) {
                let FnArg::Typed(argument) = argument else {
                    continue;
                };
                let Pat::Ident(name) = argument.pat.as_ref() else {
                    return Err(syn::Error::new_spanned(
                        argument,
                        "use named message arguments",
                    ));
                };
                if name.ident != "context" {
                    arguments.push(&name.ident);
                }
            }
            let body = &method.block;
            handlers.extend(quote! {
                impl ::polymer_command::__kameo::message::Message<::polymer_command::Queued<#message>> for #actor {
                    type Reply = #reply;
                    async fn handle(
                        &mut self,
                        queued: ::polymer_command::Queued<#message>,
                        context: &mut ::polymer_command::__kameo::message::Context<Self, Self::Reply>,
                    ) -> Self::Reply {
                        drop(queued.permit);
                        let #message { #(#arguments),* } = queued.message;
                        let _ = &context;
                        #body
                    }
                }
            });
        }
        Ok(handlers)
    }
}
