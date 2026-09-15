use std::cell::RefCell;
use std::collections::HashMap;
use std::rc::Rc;

use deno_core::error::ModuleLoaderError;
use deno_core::{
    ModuleLoadOptions, ModuleLoadReferrer, ModuleLoadResponse, ModuleLoader, ModuleSource,
    ModuleSourceCode, ModuleSpecifier, ModuleType, ResolutionKind, resolve_import,
};

#[derive(Clone)]
pub(super) struct BrowserModuleLoader {
    client: reqwest::Client,
    inline: Rc<RefCell<HashMap<ModuleSpecifier, Vec<u8>>>>,
}

impl BrowserModuleLoader {
    pub(super) fn new() -> Self {
        Self {
            client: reqwest::Client::new(),
            inline: Default::default(),
        }
    }

    fn insert_inline(&self, specifier: ModuleSpecifier, code: String) {
        self.inline
            .borrow_mut()
            .insert(specifier, code.into_bytes());
    }

    async fn load_source(
        &self,
        specifier: &ModuleSpecifier,
        module_type: ModuleType,
    ) -> Result<ModuleSource, ModuleLoaderError> {
        let (code, found) = if let Some(code) = self.inline.borrow().get(specifier).cloned() {
            (code, specifier.clone())
        } else {
            match specifier.scheme() {
                "file" => {
                    let path = specifier.to_file_path().map_err(|()| {
                        ModuleLoaderError::type_error(format!(
                            "Cannot convert module URL to a file path: {specifier}"
                        ))
                    })?;
                    let code = std::fs::read(path).map_err(ModuleLoaderError::from_err)?;
                    (code, specifier.clone())
                }
                "http" | "https" => {
                    let response = self
                        .client
                        .get(specifier.clone())
                        .send()
                        .await
                        .and_then(reqwest::Response::error_for_status)
                        .map_err(|error| ModuleLoaderError::generic(error.to_string()))?;
                    let found = ModuleSpecifier::parse(response.url().as_str())
                        .map_err(ModuleLoaderError::from_err)?;
                    let code = response
                        .bytes()
                        .await
                        .map_err(|error| ModuleLoaderError::generic(error.to_string()))?
                        .to_vec();
                    (code, found)
                }
                scheme => {
                    return Err(ModuleLoaderError::type_error(format!(
                        "Shell modules cannot be loaded from the {scheme}: scheme"
                    )));
                }
            }
        };

        Ok(ModuleSource::new_with_redirect(
            module_type,
            ModuleSourceCode::Bytes(code.into_boxed_slice().into()),
            specifier,
            &found,
            None,
        ))
    }

    async fn load_classic(&self, specifier: &ModuleSpecifier) -> Result<String, ModuleLoaderError> {
        let source = self.load_source(specifier, ModuleType::JavaScript).await?;
        match source.code {
            ModuleSourceCode::String(source) => Ok(source.to_string()),
            ModuleSourceCode::Bytes(source) => String::from_utf8(source.to_vec())
                .map_err(|error| ModuleLoaderError::type_error(error.to_string())),
        }
    }
}

impl ModuleLoader for BrowserModuleLoader {
    fn resolve(
        &self,
        specifier: &str,
        referrer: &str,
        _kind: ResolutionKind,
    ) -> Result<ModuleSpecifier, ModuleLoaderError> {
        resolve_import(specifier, referrer).map_err(ModuleLoaderError::from_err)
    }

    fn load(
        &self,
        module_specifier: &ModuleSpecifier,
        _maybe_referrer: Option<&ModuleLoadReferrer>,
        options: ModuleLoadOptions,
    ) -> ModuleLoadResponse {
        let loader = self.clone();
        let specifier = module_specifier.clone();
        let module_type = match options.requested_module_type {
            deno_core::RequestedModuleType::Json => ModuleType::Json,
            deno_core::RequestedModuleType::Text => ModuleType::Text,
            deno_core::RequestedModuleType::Bytes => ModuleType::Bytes,
            deno_core::RequestedModuleType::Other(module_type) => ModuleType::Other(module_type),
            deno_core::RequestedModuleType::None => ModuleType::JavaScript,
        };
        ModuleLoadResponse::Async(Box::pin(async move {
            loader.load_source(&specifier, module_type).await
        }))
    }
}

pub(super) enum DocumentScript {
    Classic {
        source: Option<String>,
        code: String,
    },
    Module {
        source: Option<String>,
        code: String,
    },
}

pub(super) fn document_scripts(html: &str) -> impl Iterator<Item = DocumentScript> {
    // The shell is already a trusted document selected by the frontend.
    // Script elements are collected after Blitz has parsed the complete
    // document, matching the deferred execution model used by this layer.
    let expression =
        regex::Regex::new(r"(?is)<script\b(?P<attributes>[^>]*)>(?P<source>.*?)</script\s*>")
            .expect("the script expression is valid");
    let source_attribute = regex::Regex::new(
        r#"(?is)(?:^|\s)src\s*=\s*(?:"(?P<double>[^"]*)"|'(?P<single>[^']*)'|(?P<bare>[^\s"'=<>`]+))"#,
    )
    .expect("the src expression is valid");
    let type_attribute = regex::Regex::new(
        r#"(?is)(?:^|\s)type\s*=\s*(?:"(?P<double>[^"]*)"|'(?P<single>[^']*)'|(?P<bare>[^\s"'=<>`]+))"#,
    )
    .expect("the script type expression is valid");

    expression
        .captures_iter(html)
        .filter_map(move |capture| {
            let attributes = capture.name("attributes")?.as_str();
            let source = attribute_value(&source_attribute, attributes).map(str::to_owned);
            let code = capture.name("source")?.as_str().to_owned();
            match attribute_value(&type_attribute, attributes) {
                Some(script_type) if script_type.eq_ignore_ascii_case("module") => {
                    Some(DocumentScript::Module { source, code })
                }
                None => Some(DocumentScript::Classic { source, code }),
                Some(script_type) if is_classic_script_type(script_type) => {
                    Some(DocumentScript::Classic { source, code })
                }
                _ => None,
            }
        })
        .collect::<Vec<_>>()
        .into_iter()
}

fn attribute_value<'a>(expression: &regex::Regex, attributes: &'a str) -> Option<&'a str> {
    expression.captures(attributes).and_then(|capture| {
        ["double", "single", "bare"]
            .into_iter()
            .find_map(|name| capture.name(name).map(|value| value.as_str()))
    })
}

fn is_classic_script_type(script_type: &str) -> bool {
    matches!(
        script_type.trim().to_ascii_lowercase().as_str(),
        "" | "text/javascript"
            | "application/javascript"
            | "application/ecmascript"
            | "text/ecmascript"
    )
}

pub(super) async fn execute_document_script(
    worker: &mut polymer_deno::Runtime,
    loader: &BrowserModuleLoader,
    document_url: &ModuleSpecifier,
    index: usize,
    script: DocumentScript,
) -> Result<(), Box<dyn std::error::Error>> {
    let inline_url = || {
        let mut url = document_url.clone();
        url.set_fragment(Some(&format!("script-{index}")));
        url
    };
    match script {
        DocumentScript::Classic { source, code } => {
            let (specifier, code) = if let Some(source) = source {
                let specifier = document_url.join(&source)?;
                let code = loader.load_classic(&specifier).await?;
                (specifier, code)
            } else {
                (inline_url(), code)
            };
            worker
                .js_runtime_mut()
                .execute_script(specifier.to_string(), code)?;
        }
        DocumentScript::Module { source, code } => {
            let specifier = if let Some(source) = source {
                document_url.join(&source)?
            } else {
                let specifier = inline_url();
                loader.insert_inline(specifier.clone(), code);
                specifier
            };
            worker.execute_side_module(&specifier).await?;
        }
    }
    Ok(())
}
