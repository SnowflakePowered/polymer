use consolespec::input::BindingKey;
use serde::{Deserialize, Deserializer, Serialize, Serializer, de::Error as _};
use snowglass_input_layout::layout::{InputBinding, InputLayout};
use std::{collections::HashMap, str::FromStr};

#[derive(Clone, Debug, Default, Eq, PartialEq)]
pub struct ControllerBindings(HashMap<BindingKey, Option<BindingKey>>);

impl ControllerBindings {
    pub fn new(bindings: impl IntoIterator<Item = (BindingKey, Option<BindingKey>)>) -> Self {
        Self(bindings.into_iter().collect())
    }

    pub fn iter(&self) -> impl Iterator<Item = (&BindingKey, Option<&BindingKey>)> {
        self.0
            .iter()
            .map(|(target, source)| (target, source.as_ref()))
    }

    pub fn is_empty(&self) -> bool {
        self.0.is_empty()
    }

    pub fn contains(&self, target: BindingKey) -> bool {
        self.0.contains_key(&target)
    }

    pub fn insert(&mut self, target: BindingKey, source: Option<BindingKey>) {
        self.0.insert(target, source);
    }

    pub fn remove(&mut self, target: BindingKey) {
        self.0.remove(&target);
    }

    pub fn clear(&mut self) {
        self.0.clear();
    }

    pub fn defaults(layout: &InputLayout) -> Self {
        Self::new(
            layout
                .default_gamepad_bindings()
                .map(|(target, source)| (target, Some(source))),
        )
    }

    pub fn validate(&self, layout: &InputLayout) -> Result<(), String> {
        for (target, source) in self.iter() {
            if !layout.has(*target) {
                return Err(format!(
                    "input layout {} has no element {target}",
                    layout.id
                ));
            }
            if let Some(source) = source {
                let binding = layout.custom_binding(*target, *source).ok_or_else(|| {
                    format!("input source '{source}' is not a physical gamepad expression")
                })?;
                if matches!(binding, InputBinding::Axis { .. })
                    && !layout.accepts_axis_binding(*target)
                {
                    return Err(format!("element {target} does not accept an axis binding"));
                }
            }
        }
        Ok(())
    }

    pub fn try_from_strings(
        bindings: impl IntoIterator<Item = (String, String)>,
    ) -> Result<Self, String> {
        bindings
            .into_iter()
            .map(|(target, source)| {
                let target = BindingKey::from_str(&target).map_err(|error| error.to_string())?;
                let source = match source.as_str() {
                    "none" => None,
                    _ => Some(BindingKey::from_str(&source).map_err(|error| error.to_string())?),
                };
                Ok((target, source))
            })
            .collect::<Result<HashMap<_, _>, String>>()
            .map(Self)
    }
}

impl Serialize for ControllerBindings {
    fn serialize<S>(&self, serializer: S) -> Result<S::Ok, S::Error>
    where
        S: Serializer,
    {
        self.iter()
            .map(|(target, source)| {
                (
                    target.to_string(),
                    source.map_or_else(|| "none".to_owned(), ToString::to_string),
                )
            })
            .collect::<std::collections::BTreeMap<_, _>>()
            .serialize(serializer)
    }
}

impl<'de> Deserialize<'de> for ControllerBindings {
    fn deserialize<D>(deserializer: D) -> Result<Self, D::Error>
    where
        D: Deserializer<'de>,
    {
        let bindings = HashMap::<String, String>::deserialize(deserializer)?;
        Self::try_from_strings(bindings).map_err(D::Error::custom)
    }
}
