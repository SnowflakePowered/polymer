//! Machine input topology sourced from the compile-time `consolespec` database.

use serde::Serialize;

pub use consolespec::MachineSpec;

/// Serializable input-facing view of a consolespec group.
#[derive(Clone, Debug, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct InputGroup {
    pub name: String,
    pub inputs: Vec<String>,
    pub ports: u8,
    pub accessories: bool,
}

impl InputGroup {
    pub fn accepts(&self, layout: &str) -> bool {
        self.inputs.iter().any(|id| id == layout)
    }
}

impl From<consolespec::InputGroup> for InputGroup {
    fn from(group: consolespec::InputGroup) -> Self {
        Self {
            name: group.name().to_owned(),
            inputs: group.inputs().map(str::to_owned).collect(),
            ports: group.ports(),
            accessories: group.is_accessory_group(),
        }
    }
}

/// Compatibility facade over the global compile-time machine table.
#[derive(Clone, Copy, Debug, Default)]
pub struct MachineRegistry;

impl MachineRegistry {
    pub fn embedded() -> Result<Self, String> {
        Ok(Self)
    }

    pub fn machine(&self, id: &str) -> Option<MachineSpec> {
        MachineSpec::try_from(id).ok()
    }

    pub fn machines(&self) -> impl ExactSizeIterator<Item = MachineSpec> {
        MachineSpec::all()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn exposes_compile_time_machine_topology() {
        let registry = MachineRegistry::embedded().unwrap();
        assert!(registry.machines().count() > 50);

        let wiiu = registry.machine("NINTENDO_WIIU").unwrap();
        assert_eq!(wiiu.name(), "WiiU");
        assert_eq!(
            wiiu.input_groups()
                .map(|group| group.name())
                .collect::<Vec<_>>(),
            ["gamepad", "controller", "usb"]
        );
        assert!(
            wiiu.input_group("gamepad")
                .unwrap()
                .accepts("WIIU_DRC_CONTROLLER")
        );
    }

    #[test]
    fn a_machine_may_declare_no_input() {
        assert_eq!(
            MachineSpec::try_from("NINTENDO_FDS")
                .unwrap()
                .input_groups()
                .len(),
            0
        );
    }
}
