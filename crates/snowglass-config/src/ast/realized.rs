use std::sync::Arc;

use crate::arena::Handle;

use super::{Choice, ConfigurationDefinition, DomainDefinition, OptionDefinition, RealizedOptionRef, Value};

/// The context-dependent overlay used both by the frontend and by instances.
pub struct RealizedDefinition {
    definition: Arc<ConfigurationDefinition>,
    options: Vec<RealizedOption>,
}

impl ConfigurationDefinition {
    /// Realizes every option with a caller-supplied resolver.
    pub fn resolve_with<E>(
        self: &Arc<Self>,
        mut realize: impl FnMut(
            Handle<OptionDefinition>,
            &OptionDefinition,
        ) -> Result<RealizedOption, E>,
    ) -> Result<Arc<RealizedDefinition>, E> {
        let mut options = Vec::with_capacity(self.options.len());
        for (handle, option) in self.options.iter() {
            options.push(realize(handle, option)?);
        }

        Ok(Arc::new(RealizedDefinition {
            definition: self.clone(),
            options,
        }))
    }
}

impl RealizedDefinition {
    pub fn definition(&self) -> &ConfigurationDefinition {
        &self.definition
    }

    pub fn option(&self, handle: Handle<OptionDefinition>) -> RealizedOptionRef<'_> {
        RealizedOptionRef {
            definition: &self.definition.options[handle],
            realized: &self.options[handle.index()],
        }
    }

    pub fn root_sections(
        &self,
    ) -> impl ExactSizeIterator<Item = Handle<super::SectionDefinition>> + '_ {
        self.definition.root_sections.iter().copied()
    }

    pub fn choices(&self, handle: Handle<OptionDefinition>) -> Option<&[Choice]> {
        match &self.options[handle.index()].domain {
            RealizedDomain::None => None,
            RealizedDomain::Static => match &self.definition.options[handle].value {
                super::ValueDefinition::Select {
                    domain: DomainDefinition::Static(choices),
                    ..
                } => Some(choices),
                _ => None,
            },
            RealizedDomain::Dynamic(choices) => Some(choices),
        }
    }

    pub(crate) fn realized_option(&self, handle: Handle<OptionDefinition>) -> &RealizedOption {
        &self.options[handle.index()]
    }
}

#[derive(Clone, Debug)]
pub struct RealizedOption {
    /// `Value::Null` represents an option for which no default was available.
    pub default: Value,
    pub domain: RealizedDomain,
}

#[derive(Clone, Debug)]
pub enum RealizedDomain {
    None,
    /// Reuse the choices owned by the underlying static definition.
    Static,
    Dynamic(Box<[Choice]>),
}
