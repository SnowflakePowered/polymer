use consolespec::input::BindingKey;
use serde::{Deserialize, Deserializer, Serialize, Serializer, de::Error as _};
use std::{fmt, str::FromStr};

#[derive(Clone, Copy, Debug, Eq, Hash, PartialEq)]
pub enum PointerAxis {
    X,
    Y,
}

impl PointerAxis {
    pub const fn index(self) -> usize {
        match self {
            Self::X => 0,
            Self::Y => 1,
        }
    }

    const fn name(self) -> &'static str {
        match self {
            Self::X => "x",
            Self::Y => "y",
        }
    }

    fn parse(text: &str) -> Option<Self> {
        match text {
            "x" => Some(Self::X),
            "y" => Some(Self::Y),
            _ => None,
        }
    }
}

/// A physical control that can drive a layout target.
///
/// The wire form is a dotted string: a standard pad key (`button.a`),
/// `key.<DOM code>`, `mouse.button.<n>`, `mouse.delta.<x|y>` or
/// `mouse.position.<x|y>`.
#[derive(Clone, Debug, Eq, Hash, PartialEq)]
pub enum InputSource {
    Pad(BindingKey),
    Key(String),
    MouseButton(u8),
    MouseDelta(PointerAxis),
    MousePosition(PointerAxis),
}

impl fmt::Display for InputSource {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Pad(key) => write!(formatter, "{key}"),
            Self::Key(code) => write!(formatter, "key.{code}"),
            Self::MouseButton(index) => write!(formatter, "mouse.button.{index}"),
            Self::MouseDelta(axis) => write!(formatter, "mouse.delta.{}", axis.name()),
            Self::MousePosition(axis) => write!(formatter, "mouse.position.{}", axis.name()),
        }
    }
}

impl FromStr for InputSource {
    type Err = String;

    fn from_str(text: &str) -> Result<Self, Self::Err> {
        let invalid = || format!("invalid input source '{text}'");
        if let Some(code) = text.strip_prefix("key.") {
            return if code.is_empty() {
                Err(invalid())
            } else {
                Ok(Self::Key(code.to_owned()))
            };
        }
        if let Some(rest) = text.strip_prefix("mouse.") {
            return match rest.split_once('.') {
                Some(("button", index)) => {
                    index.parse().map(Self::MouseButton).map_err(|_| invalid())
                }
                Some(("delta", axis)) => PointerAxis::parse(axis)
                    .map(Self::MouseDelta)
                    .ok_or_else(invalid),
                Some(("position", axis)) => PointerAxis::parse(axis)
                    .map(Self::MousePosition)
                    .ok_or_else(invalid),
                _ => Err(invalid()),
            };
        }
        BindingKey::from_str(text)
            .map(Self::Pad)
            .map_err(|error| format!("{invalid}: {error}", invalid = invalid()))
    }
}

impl Serialize for InputSource {
    fn serialize<S: Serializer>(&self, serializer: S) -> Result<S::Ok, S::Error> {
        serializer.collect_str(self)
    }
}

impl<'de> Deserialize<'de> for InputSource {
    fn deserialize<D: Deserializer<'de>>(deserializer: D) -> Result<Self, D::Error> {
        let text = <std::borrow::Cow<'de, str>>::deserialize(deserializer)?;
        text.parse().map_err(D::Error::custom)
    }
}
