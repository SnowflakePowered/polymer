//! Projection of a device's controls onto Polymer's standard gamepad.
//!
//! This is mapping, not description. A device says what controls it has; this
//! decides which of the standard gamepad's controls carry them. Both sides are
//! [`BindingKey`]s — the left of the device, the right of the standard pad —
//! so there is no third vocabulary of slot names, and the same table read
//! backwards says which of *our* controls a physical pad's button is.
//!
//! Keeping it in one shared table means "which physical button is C-Up" is the
//! same in every emulator, and that the six-slot overflow budget can be checked
//! globally — a per-plugin table could not tell you that two plugins had
//! allocated the same slot differently.
//!
//! The allocation follows the conventional gamepad layout: unused standard slots
//! are filled before any spare. A Wii Remote's 1 and 2 become X and Y because a
//! Wii Remote has no X/Y face buttons; a Saturn pad's C and Z become shoulders
//! because its shoulder slots are free.

use consolespec::{InputSpec, input::BindingKey};
use std::collections::{BTreeMap, HashMap};
use std::fmt;
use std::str::FromStr;
use std::sync::OnceLock;
use toml_spanner::Arena;

use crate::error::{Error, Result};

use super::gamepad::StandardGamepad;

pub const EMBEDDED_SDL_PROJECTION: &str = include_str!("../../projection/SDL.proj.toml");

/// A way off the pad. Some controls are not gamepad controls at all, and saying
/// *which* transport carries them is what lets one be routed later.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum Transport {
    /// A 5200's or an Intellivision's keypad is a keypad; the emulator reads it
    /// as keys, not as pad buttons.
    Keyboard,
}

impl Transport {
    fn parse(name: &str) -> Option<Self> {
        match name {
            "keyboard" => Some(Self::Keyboard),
            _ => None,
        }
    }
}

impl fmt::Display for Transport {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Keyboard => formatter.write_str("keyboard"),
        }
    }
}

/// Where one of a device's controls goes.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum Target {
    /// The standard gamepad control that carries it.
    Key(BindingKey),
    /// Two pad controls at once: an eight-way stick's north-east is the pad's
    /// up *and* right, and an absolute pointer is both raw axes.
    Both(BindingKey, BindingKey),
    /// Off the pad, but reached: the named transport carries it instead.
    Transport(Transport),
    /// Off the pad and reached by nothing. The model presents one pointer, so a device
    /// with two mice can only place one. Distinct from an absent entry, which is
    /// an oversight rather than a decision.
    None,
}

#[derive(Clone, Debug)]
pub struct Projection {
    /// Layout id -> device key -> where it goes.
    layouts: HashMap<String, HashMap<BindingKey, Target>>,
}

impl Projection {
    pub fn embedded() -> Result<Self> {
        Self::from_toml(EMBEDDED_SDL_PROJECTION)
    }

    /// The embedded table, parsed once.
    ///
    /// Like the layouts it maps, this is fixed data compiled into the binary, so
    /// there is nothing to own and no reason for a caller deep in the data plane
    /// to have been handed one.
    pub fn shared() -> &'static Self {
        static SHARED: OnceLock<Projection> = OnceLock::new();
        SHARED.get_or_init(|| {
            Self::embedded()
                .unwrap_or_else(|error| panic!("the standard projection is unusable: {error}"))
        })
    }

    pub(crate) fn from_toml(source: &str) -> Result<Self> {
        let arena = Arena::new();
        let mut document = toml_spanner::parse(source, &arena)?;
        let document = document.to::<BTreeMap<&str, BTreeMap<&str, &str>>>()?;
        let mut layouts = HashMap::new();
        for (layout, table) in document {
            let mut entries = HashMap::new();
            for (key, target) in table {
                let key = BindingKey::from_str(key)?;
                let target = if target == "none" {
                    Target::None
                } else if let Some(transport) = Transport::parse(target) {
                    Target::Transport(transport)
                } else {
                    let target = BindingKey::from_str(target)?;
                    // A target the pad does not have could never fire, and is
                    // the mistake this file is most able to make.
                    if !StandardGamepad::has(target) {
                        return Err(Error::projection(format!(
                            "projection {layout}.{key}: the standard gamepad has no {target}"
                        )));
                    }
                    Target::Key(target)
                };
                entries.insert(key, target);
            }
            layouts.insert(layout.to_owned(), entries);
        }
        Ok(Self { layouts })
    }

    /// Where one control of `layout` lands.
    ///
    /// An explicit entry wins. Otherwise the control keeps its own name, if the
    /// pad has it — which is what lets the table list only the differences.
    /// Identity is resolved against the device, so a lone D-pad's
    /// `directional.left.n` reaches the pad's `directional.n`, while a *second*
    /// directional does not: the N64's C-buttons are not its D-pad, and must
    /// say where they go.
    pub fn target(&self, layout_id: &str, key: BindingKey) -> Option<Target> {
        if let Some(target) = self
            .layouts
            .get(layout_id)
            .and_then(|table| table.get(&key))
        {
            return Some(*target);
        }
        // A control the pad has under the same name keeps it.
        if StandardGamepad::has(key) {
            return Some(Self::place(key));
        }
        // Otherwise the device's *primary* cluster of that kind answers to the
        // pad's unqualified name for it, and no other does.
        let unaligned = key.unaligned();
        if resolve(layout_id, unaligned) == Some(key) && StandardGamepad::has(unaligned) {
            return Some(Self::place(unaligned));
        }
        None
    }

    /// One pad control, or the two a composite splits into.
    fn place(target: BindingKey) -> Target {
        match StandardGamepad::decompose(target) {
            Some([first, second]) => Target::Both(first, second),
            None => Target::Key(target),
        }
    }
}

fn resolve(layout_id: &str, key: BindingKey) -> Option<BindingKey> {
    let layout = InputSpec::try_from(layout_id).ok()?;
    if layout.elements().any(|element| element.binding() == key) {
        return Some(key);
    }
    let (kind, class) = match key {
        BindingKey::Directional {
            alignment: None, ..
        } => ("directional", None),
        BindingKey::Analog {
            class,
            alignment: None,
            ..
        } => ("analog", Some(class)),
        BindingKey::Pointer {
            alignment: None, ..
        } => ("pointer", None),
        _ => return None,
    };
    let cluster = layout
        .clusters()
        .find(|cluster| cluster.kind() == kind && cluster.class() == class)?;
    let candidate = key.with_alignment(cluster.alignment());
    layout
        .elements()
        .any(|element| element.binding() == candidate)
        .then_some(candidate)
}
