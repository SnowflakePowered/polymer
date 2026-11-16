use std::{borrow::Cow, fmt, io::Write, path::Path};

use rt_format::{Format, FormatArgument, NoNamedArguments, ParsedFormat, Specifier};

use crate::{
    ast::{BindingValue, BoolStyleDefinition, TargetOutputDefinition},
    error::{Error, Result},
};

mod bml;
mod ini;
mod toml;
mod xml;
mod yaml;

pub use bml::BmlEmitter;
pub use ini::{IniEmitter, IniOptions, IniQuote};
pub use toml::TomlEmitter;
pub use xml::XmlEmitter;
pub use yaml::YamlEmitter;

/// Per-format options for one render, keyed by output format.
#[derive(Clone, Copy, Debug, Default, serde::Deserialize)]
#[serde(default, deny_unknown_fields)]
pub struct RenderOptions {
    pub ini: IniOptions,
}

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
    /// Whether this section is one of several same-named siblings.
    pub repeated: bool,
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

/// A choice has already been mapped to its emulator-facing scalar by the time
/// it reaches an emitter. A list only reaches formats that can hold one.
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
    Float {
        value: f64,
        style: Option<&'a str>,
    },
    String(&'a str),
    Path(&'a Path),
    List(&'a [Box<str>]),
}

impl<'a> From<&'a BindingValue> for ScalarValue<'a> {
    fn from(value: &'a BindingValue) -> Self {
        match value {
            BindingValue::Text(text) => Self::String(text),
            BindingValue::Integer(value) => Self::Integer {
                value: *value,
                style: None,
            },
            BindingValue::List(items) => Self::List(items),
        }
    }
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
            } => Cow::Owned(NumberArgument::Integer(value).render(style)?),
            Self::Float { value, style: None } => Cow::Owned(value.to_string()),
            Self::Float {
                value,
                style: Some(style),
            } => Cow::Owned(NumberArgument::Float(value).render(style)?),
            Self::String(value) => Cow::Borrowed(value),
            Self::Path(value) => Cow::Borrowed(
                value
                    .to_str()
                    .ok_or_else(|| Error::NonUtf8Path(value.to_owned()))?,
            ),
            Self::List(_) => {
                return Err(Error::emitter_message(
                    "this format cannot represent a list value",
                ));
            }
        })
    }
}

/// A number rendered through a runtime format string such as `{:.2}` or `{:#x}`.
#[derive(Debug)]
enum NumberArgument {
    Integer(i64),
    Float(f64),
}

impl NumberArgument {
    fn render(self, style: &str) -> EmitResult<String> {
        let arguments = [self];
        let format = ParsedFormat::parse(style, &arguments, &NoNamedArguments).map_err(|offset| {
            Error::emitter_message(format!(
                "invalid number style `{style}` at byte offset {offset}"
            ))
        })?;
        Ok(format.to_string())
    }

    fn integer(
        &self,
        formatter: &mut fmt::Formatter<'_>,
        format: fn(&i64, &mut fmt::Formatter<'_>) -> fmt::Result,
    ) -> fmt::Result {
        match self {
            Self::Integer(value) => format(value, formatter),
            Self::Float(_) => Err(fmt::Error),
        }
    }
}

impl FormatArgument for NumberArgument {
    fn supports_format(&self, specifier: &Specifier) -> bool {
        matches!(self, Self::Integer(_))
            || matches!(
                specifier.format,
                Format::Display | Format::Debug | Format::LowerExp | Format::UpperExp
            )
    }

    fn fmt_display(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Integer(value) => fmt::Display::fmt(value, formatter),
            Self::Float(value) => fmt::Display::fmt(value, formatter),
        }
    }

    fn fmt_debug(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Integer(value) => fmt::Debug::fmt(value, formatter),
            Self::Float(value) => fmt::Debug::fmt(value, formatter),
        }
    }

    fn fmt_octal(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        self.integer(formatter, fmt::Octal::fmt)
    }

    fn fmt_lower_hex(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        self.integer(formatter, fmt::LowerHex::fmt)
    }

    fn fmt_upper_hex(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        self.integer(formatter, fmt::UpperHex::fmt)
    }

    fn fmt_binary(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        self.integer(formatter, fmt::Binary::fmt)
    }

    fn fmt_lower_exp(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Integer(value) => fmt::LowerExp::fmt(value, formatter),
            Self::Float(value) => fmt::LowerExp::fmt(value, formatter),
        }
    }

    fn fmt_upper_exp(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Integer(value) => fmt::UpperExp::fmt(value, formatter),
            Self::Float(value) => fmt::UpperExp::fmt(value, formatter),
        }
    }

    fn to_usize(&self) -> std::result::Result<usize, ()> {
        match self {
            Self::Integer(value) => usize::try_from(*value).map_err(|_| ()),
            Self::Float(_) => Err(()),
        }
    }
}
