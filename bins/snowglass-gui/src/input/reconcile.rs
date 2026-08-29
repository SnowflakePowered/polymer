use consolespec::input::BindingKey;
use snowglass_input::{
    hub::{BindingOverride, InputConfiguration, SourceControl, SourceSelector},
    provider::sdl3::{Sdl3Provider, SlotConfig},
};
use snowglass_runtime::ResolvedInputConfiguration;

pub(super) struct InputReconciler;

impl InputReconciler {
    pub(super) fn reconcile(
        provider: &Sdl3Provider,
        previous: &ResolvedInputConfiguration,
        next: &ResolvedInputConfiguration,
        topology_locked: bool,
    ) -> Result<(), String> {
        Self::validate(previous, next, topology_locked)?;
        Self::apply_reconciliation(provider, previous, next)
    }

    pub(super) fn validate(
        previous: &ResolvedInputConfiguration,
        next: &ResolvedInputConfiguration,
        topology_locked: bool,
    ) -> Result<(), String> {
        if topology_locked && Self::topology_changed(&previous.configuration, &next.configuration) {
            return Err(
                "the active emulator plugin does not support runtime controller topology changes"
                    .to_owned(),
            );
        }
        Self::validate_bindings(next)?;

        Ok(())
    }

    pub(super) fn apply_reconciliation(
        provider: &Sdl3Provider,
        previous: &ResolvedInputConfiguration,
        next: &ResolvedInputConfiguration,
    ) -> Result<(), String> {
        let previous_slots = Self::slots(previous);
        let next_slots = Self::slots(next);
        if previous_slots != next_slots {
            provider.reconfigure_slots(next_slots)?;
        }
        Self::project_bindings(provider, next);
        Ok(())
    }

    pub(super) fn apply(
        provider: &Sdl3Provider,
        configuration: &ResolvedInputConfiguration,
    ) -> Result<(), String> {
        Self::validate_bindings(configuration)?;
        provider.reconfigure_slots(Self::slots(configuration))?;
        Self::project_bindings(provider, configuration);
        Ok(())
    }

    fn topology_changed(previous: &InputConfiguration, next: &InputConfiguration) -> bool {
        previous.machine != next.machine
            || previous.ports.len() != next.ports.len()
            || previous.ports.iter().zip(&next.ports).any(|(old, new)| {
                old.port != new.port || old.layout != new.layout || old.slot != new.slot
            })
    }

    fn validate_bindings(configuration: &ResolvedInputConfiguration) -> Result<(), String> {
        for port in &configuration.configuration.ports {
            let source = Self::source_for_port(&configuration.configuration, port.slot);
            let Some(slot) = port.slot.and_then(|slot| configuration.slots.get(&slot)) else {
                continue;
            };
            if let Some(binding) = slot
                .overrides
                .values()
                .find(|binding| !Self::binding_supported(binding, source))
            {
                return Err(format!(
                    "binding {binding:?} cannot yet be projected by the SDL data plane"
                ));
            }
        }
        Ok(())
    }

    pub(super) fn binding_supported(
        binding: &BindingOverride,
        source: Option<&SourceSelector>,
    ) -> bool {
        if matches!(
            binding,
            BindingOverride::Axis {
                source: SourceControl::MouseDelta { .. } | SourceControl::MousePosition { .. },
                ..
            }
        ) {
            return true;
        }
        if matches!(source, Some(SourceSelector::KeyboardMouse)) {
            return matches!(
                binding,
                BindingOverride::Button {
                    source: SourceControl::Key { .. }
                } | BindingOverride::Axis {
                    source: SourceControl::Key { .. },
                    ..
                } | BindingOverride::Disabled
            );
        }
        binding.sdl_binding().is_some()
    }

    pub(super) fn slots(configuration: &ResolvedInputConfiguration) -> Vec<SlotConfig> {
        let highest_slot = configuration
            .configuration
            .ports
            .iter()
            .filter_map(|port| port.slot)
            .map(usize::from)
            .max();
        let mut slots = highest_slot
            .map(|slot| {
                vec![
                    SlotConfig {
                        source: None,
                        layout_id: "XBOX_CONTROLLER".to_owned(),
                        force_mouse: false,
                        projection_mode: snowglass_input::layout::ProjectionMode::Position,
                    };
                    slot + 1
                ]
            })
            .unwrap_or_default();
        for port in &configuration.configuration.ports {
            let Some(slot) = port.slot else { continue };
            let source = Self::source_for_port(&configuration.configuration, Some(slot));
            let mapping = configuration
                .slots
                .get(&slot)
                .expect("every configured virtual slot has a resolved device mapping");
            slots[usize::from(slot)] = SlotConfig {
                source: source.cloned(),
                layout_id: port.layout.clone(),
                force_mouse: port.force_mouse
                    || matches!(source, Some(SourceSelector::KeyboardMouse)),
                projection_mode: mapping.projection_mode,
            };
        }
        slots
    }

    fn source_for_port(
        configuration: &InputConfiguration,
        slot: Option<u8>,
    ) -> Option<&SourceSelector> {
        slot.and_then(|index| {
            configuration
                .assignments
                .iter()
                .find(|assignment| assignment.index == index)
                .map(|assignment| &assignment.source)
        })
    }

    fn project_bindings(provider: &Sdl3Provider, configuration: &ResolvedInputConfiguration) {
        for port in &configuration.configuration.ports {
            let Some(slot) = port.slot.map(usize::from) else {
                continue;
            };
            provider.reset_bindings(slot);
            let source = Self::source_for_port(&configuration.configuration, port.slot);
            let Some(mapping) = port.slot.and_then(|slot| configuration.slots.get(&slot)) else {
                continue;
            };
            for (target, binding) in &mapping.overrides {
                Self::project_binding(provider, slot, target, binding, source);
            }
        }
    }

    fn project_binding(
        provider: &Sdl3Provider,
        slot: usize,
        target: &BindingKey,
        binding: &BindingOverride,
        source: Option<&SourceSelector>,
    ) {
        if let BindingOverride::Axis {
            source:
                pointer @ (SourceControl::MouseDelta { .. } | SourceControl::MousePosition { .. }),
            transform,
        } = binding
        {
            provider.set_mouse_axis_binding(slot, *target, pointer.clone(), *transform);
        } else if matches!(source, Some(SourceSelector::KeyboardMouse)) {
            let key = match binding {
                BindingOverride::Button {
                    source: SourceControl::Key { code },
                }
                | BindingOverride::Axis {
                    source: SourceControl::Key { code },
                    ..
                } => Some(code.clone()),
                BindingOverride::Disabled => None,
                _ => return,
            };
            provider.set_key_binding(slot, *target, key);
        } else if let Some(binding) = binding.sdl_binding() {
            provider.set_binding(slot, *target, binding);
        }
    }
}
