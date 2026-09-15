use std::{borrow::Cow, fmt, io::Write, path::Path};

use rt_format::{FormatArgument, NoNamedArguments, ParsedFormat, Specifier};

use crate::{
    ast::{BoolStyleDefinition, TargetOutputDefinition},
    error::{Error, Result},
};

mod bml;
mod ini;
mod xml;

pub use bml::BmlEmitter;
pub use ini::IniEmitter;
pub use xml::XmlEmitter;

/// A format-specific, in-memory document builder driven by
/// [`Serializer`](super::Serializer).
///
/// Implementations own whatever representation best suits their format: a
/// third-party document type, a tree, a map, or a custom buffer. `finish` is
/// called once after the balanced section callbacks and flushes the completed
/// document to the supplied writer.
pub trait Emitter {
    fn begin_document(&mut self, _document: DocumentRef<'_>) -> EmitResult<()> {
        Ok(())
    }

    fn enter_section(&mut self, section: SectionRef<'_>) -> EmitResult<()>;

    fn write_value(&mut self, value: EmittedValue<'_>) -> EmitResult<()>;

    /// Writes one emulator input binding. Structured formats may override this.
    fn write_binding(&mut self, binding: EmittedBinding<'_>) -> EmitResult<()> {
        self.write_value(EmittedValue {
            key: binding.key,
            value: ScalarValue::String(&binding.value),
        })
    }

    fn exit_section(&mut self, section: SectionRef<'_>) -> EmitResult<()>;

    fn end_document(&mut self, _document: DocumentRef<'_>) -> EmitResult<()> {
        Ok(())
    }

    /// Finalizes the in-memory representation and encodes it in one step.
    fn finish(&mut self, writer: &mut dyn Write) -> EmitResult<()>;
}

pub type EmitResult<T> = Result<T>;

#[derive(Clone, Copy, Debug)]
pub struct DocumentRef<'a> {
    pub target: &'a str,
    pub output: &'a TargetOutputDefinition,
}

/// A target node represented as a section in the output format.
#[derive(Clone, Copy, Debug)]
pub struct SectionRef<'a> {
    pub path: &'a str,
    pub name: &'a str,
    pub attributes: &'a [EmittedAttribute<'a>],
}

impl SectionRef<'_> {
    pub fn reject_attributes(&self, format: &str) -> EmitResult<()> {
        if self.attributes.is_empty() {
            Ok(())
        } else {
            Err(Error::emitter_message(format!(
                "{format} sections do not support attributes"
            )))
        }
    }
}

#[derive(Clone, Copy, Debug)]
pub struct EmittedAttribute<'a> {
    pub key: &'a str,
    pub value: ScalarValue<'a>,
}

#[derive(Clone, Copy, Debug)]
pub struct EmittedValue<'a> {
    pub key: &'a str,
    pub value: ScalarValue<'a>,
}

#[derive(Clone, Debug)]
pub struct EmittedBinding<'a> {
    pub key: &'a str,
    pub value: Cow<'a, str>,
}

/// A choice has already been mapped to its emulator-facing scalar by the time
/// it reaches an emitter.
#[derive(Clone, Copy, Debug)]
pub enum ScalarValue<'a> {
    Boolean {
        value: bool,
        style: Option<&'a BoolStyleDefinition>,
    },
    Integer {
        value: i64,
        style: Option<&'a str>,
    },
    Float(f64),
    String(&'a str),
    Path(&'a Path),
}

impl<'a> ScalarValue<'a> {
    /// Applies definition-level scalar representation independently of the
    /// surrounding document format.
    pub fn render(self) -> EmitResult<Cow<'a, str>> {
        Ok(match self {
            Self::Boolean { value, style } => style.map_or_else(
                || Cow::Owned(value.to_string()),
                |style| {
                    Cow::Borrowed(if value {
                        style.r#true.as_ref()
                    } else {
                        style.r#false.as_ref()
                    })
                },
            ),
            Self::Integer { value, style: None } => Cow::Owned(value.to_string()),
            Self::Integer {
                value,
                style: Some(style),
            } => Cow::Owned(render_integer(value, style)?),
            Self::Float(value) => Cow::Owned(value.to_string()),
            Self::String(value) => Cow::Borrowed(value),
            Self::Path(value) => Cow::Borrowed(
                value
                    .to_str()
                    .ok_or_else(|| Error::NonUtf8Path(value.to_owned()))?,
            ),
        })
    }
}

fn render_integer(value: i64, style: &str) -> EmitResult<String> {
    let arguments = [IntegerArgument(value)];
    let format = ParsedFormat::parse(style, &arguments, &NoNamedArguments).map_err(|offset| {
        Error::emitter_message(format!(
            "invalid integer style `{style}` at byte offset {offset}"
        ))
    })?;
    Ok(format.to_string())
}

#[derive(Debug)]
struct IntegerArgument(i64);

impl FormatArgument for IntegerArgument {
    fn supports_format(&self, _specifier: &Specifier) -> bool {
        true
    }

    fn fmt_display(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        fmt::Display::fmt(&self.0, formatter)
    }

    fn fmt_debug(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        fmt::Debug::fmt(&self.0, formatter)
    }

    fn fmt_octal(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        fmt::Octal::fmt(&self.0, formatter)
    }

    fn fmt_lower_hex(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        fmt::LowerHex::fmt(&self.0, formatter)
    }

    fn fmt_upper_hex(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        fmt::UpperHex::fmt(&self.0, formatter)
    }

    fn fmt_binary(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        fmt::Binary::fmt(&self.0, formatter)
    }

    fn fmt_lower_exp(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        fmt::LowerExp::fmt(&self.0, formatter)
    }

    fn fmt_upper_exp(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        fmt::UpperExp::fmt(&self.0, formatter)
    }

    fn to_usize(&self) -> std::result::Result<usize, ()> {
        usize::try_from(self.0).map_err(|_| ())
    }
}
