use super::ShellDocument;
use crate::shell_dom::ShellHitSnapshot;
use crate::shell_runtime::ShellRuntime;
use blitz_dom::{EventDriver, EventHandler, NodeId};
use blitz_traits::events::{
    BlitzImeEvent, BlitzKeyEvent, BlitzPointerEvent, BlitzPointerId, BlitzWheelDelta,
    BlitzWheelEvent, DomEvent, DomEventData, EventState, KeyState, MouseEventButton,
    MouseEventButtons, Point, PointerCoords, PointerDetails, UiEvent,
};
use std::{cell::Cell, collections::HashMap, rc::Rc, str::FromStr};
use style_dom::ElementState;

struct DenoEventHandler<'a> {
    runtime: &'a mut ShellRuntime,
    dom_to_script: &'a HashMap<NodeId, usize>,
    claimed: Rc<Cell<bool>>,
    ordinary_dom_hit: bool,
    target_override: Option<usize>,
    default_prevented: Option<Rc<Cell<bool>>>,
}

impl EventHandler for DenoEventHandler<'_> {
    fn handle_event(
        &mut self,
        chain: &[NodeId],
        event: &mut DomEvent,
        _document: &mut dyn blitz_dom::Document,
        event_state: &mut EventState,
    ) {
        let target = self.target_override.unwrap_or_else(|| {
            chain
                .iter()
                .find_map(|node| self.dom_to_script.get(node).copied())
                .unwrap_or(event.target.as_u64() as usize)
        });
        let result = self.runtime.dispatch_event(
            event.name(),
            target,
            &event.data,
            event.bubbles,
            event.cancelable,
        );
        if result.default_prevented {
            if let Some(default_prevented) = &self.default_prevented {
                default_prevented.set(true);
            }
            event_state.prevent_default();
        }
        // Blitz's painter supports hoisted transformed descendants that its
        // hit tester does not yet resolve. If Polymer retargeted this event
        // to the paint-order hit, a native click default against Blitz's
        // different target would be incorrect. Compatibility events and click
        // synthesis still remain owned by EventDriver.
        if self.target_override.is_some() && matches!(event.data, DomEventData::Click(_)) {
            event_state.prevent_default();
        }
        if result.propagation_stopped {
            event_state.stop_propagation();
        }
        if result.default_prevented || (result.handled && self.ordinary_dom_hit) {
            self.claimed.set(true);
        }
    }
}

pub struct DomMouseEvent<'a> {
    pub kind: &'a str,
    pub x: f32,
    pub y: f32,
    pub button: u8,
    pub buttons: u8,
    pub ctrl_key: bool,
    pub shift_key: bool,
}

pub struct DomKeyboardEvent<'a> {
    pub kind: &'a str,
    pub key: String,
    pub code: String,
    pub repeat: bool,
    pub alt_key: bool,
    pub ctrl_key: bool,
    pub shift_key: bool,
}

pub struct DomWheelEvent {
    pub x: f32,
    pub y: f32,
    pub horizontal: f64,
    pub vertical: f64,
    pub ctrl_key: bool,
    pub shift_key: bool,
    pub alt_key: bool,
}

impl ShellDocument {
    fn set_pointer_hover(&mut self, target: Option<NodeId>) {
        let next_path = target
            .filter(|target| self.document.as_ref().get_node(*target).is_some())
            .map(|target| self.document.as_ref().node_chain(target))
            .unwrap_or_default();
        if next_path == self.pointer_hover_path {
            return;
        }
        let previous_path = std::mem::take(&mut self.pointer_hover_path);
        for node in previous_path
            .iter()
            .copied()
            .filter(|node| !next_path.contains(node))
        {
            if self.document.as_ref().get_node(node).is_some() {
                self.document
                    .as_mut()
                    .snapshot_node_and(node, ElementState::HOVER, |node| node.unhover());
            }
        }
        for node in next_path
            .iter()
            .copied()
            .filter(|node| !previous_path.contains(node))
        {
            self.document
                .as_mut()
                .snapshot_node_and(node, ElementState::HOVER, |node| node.hover());
        }
        self.pointer_hover_path = next_path;
        self.dirty = true;
        self.needs_resolve = true;
        self.needs_geometry_sync = true;
    }

    pub fn clear_pointer_hover(&mut self) {
        let native_changed = self.document.as_mut().clear_hover();
        self.set_pointer_hover(None);
        if native_changed {
            self.dirty = true;
            self.needs_resolve = true;
            self.needs_geometry_sync = true;
        }
    }

    /// Dispatches a mouse event to a target selected from a shell hit
    /// snapshot. The return value is true when script claims the pointer from
    /// the guest surface below this layer.
    pub fn mouse_event(
        &mut self,
        runtime: &mut ShellRuntime,
        hit: ShellHitSnapshot,
        event: DomMouseEvent<'_>,
    ) -> bool {
        let DomMouseEvent {
            kind,
            x,
            y,
            button,
            buttons,
            ctrl_key,
            shift_key,
        } = event;
        let data = BlitzPointerEvent {
            id: BlitzPointerId::Mouse,
            is_primary: true,
            coords: PointerCoords {
                page_x: x,
                page_y: y,
                screen_x: x,
                screen_y: y,
                client_x: x,
                client_y: y,
            },
            button: match button {
                1 => MouseEventButton::Auxiliary,
                2 => MouseEventButton::Secondary,
                3 => MouseEventButton::Fourth,
                4 => MouseEventButton::Fifth,
                _ => MouseEventButton::Main,
            },
            buttons: MouseEventButtons::from_bits_truncate(buttons),
            mods: {
                let mut modifiers = keyboard_types::Modifiers::empty();
                modifiers.set(keyboard_types::Modifiers::CONTROL, ctrl_key);
                modifiers.set(keyboard_types::Modifiers::SHIFT, shift_key);
                modifiers
            },
            details: PointerDetails::default(),
            element: Point::default(),
            active_pointers: Default::default(),
        };
        let event = match kind {
            "mousedown" => Some(UiEvent::PointerDown(data)),
            "mouseup" => Some(UiEvent::PointerUp(data)),
            "mousemove" => Some(UiEvent::PointerMove(data)),
            _ => None,
        };
        let claimed = Rc::new(Cell::new(false));
        if let Some(event) = event {
            let native_script_target = self.document.as_ref().hit(x, y).and_then(|native| {
                std::iter::successors(Some(native.node_id), |node| {
                    self.document
                        .as_ref()
                        .get_node(*node)
                        .and_then(|node| node.parent)
                })
                .find_map(|node| self.dom_to_script.get(&node).copied())
            });
            let target_override = self
                .dom_to_script
                .get(&hit.node_id)
                .copied()
                .filter(|target| Some(*target) != native_script_target);
            let handler = DenoEventHandler {
                runtime,
                dom_to_script: &self.dom_to_script,
                claimed: claimed.clone(),
                ordinary_dom_hit: !hit.over_tracked_surface,
                target_override,
                default_prevented: None,
            };
            EventDriver::new(&mut self.document, handler).handle_ui_event(event);
            if target_override.is_some() {
                let native_changed = self.document.as_mut().clear_hover();
                self.set_pointer_hover(Some(hit.node_id));
                if native_changed {
                    self.dirty = true;
                    self.needs_resolve = true;
                    self.needs_geometry_sync = true;
                }
            } else {
                self.set_pointer_hover(None);
            }
        }
        self.apply_script_mutations(runtime);
        claimed.get()
    }

    pub fn wheel_event(
        &mut self,
        runtime: &mut ShellRuntime,
        hit: ShellHitSnapshot,
        event: DomWheelEvent,
    ) -> bool {
        let mut modifiers = keyboard_types::Modifiers::empty();
        modifiers.set(keyboard_types::Modifiers::ALT, event.alt_key);
        modifiers.set(keyboard_types::Modifiers::CONTROL, event.ctrl_key);
        modifiers.set(keyboard_types::Modifiers::SHIFT, event.shift_key);
        let claimed = Rc::new(Cell::new(false));
        let default_prevented = Rc::new(Cell::new(false));
        let handler = DenoEventHandler {
            runtime,
            dom_to_script: &self.dom_to_script,
            claimed: claimed.clone(),
            ordinary_dom_hit: !hit.over_tracked_surface,
            target_override: None,
            default_prevented: Some(default_prevented.clone()),
        };
        EventDriver::new(&mut self.document, handler).handle_dom_event(DomEvent::new(
            hit.node_id,
            DomEventData::Wheel(BlitzWheelEvent {
                delta: BlitzWheelDelta::Pixels(event.horizontal, event.vertical),
                coords: PointerCoords {
                    page_x: event.x,
                    page_y: event.y,
                    screen_x: event.x,
                    screen_y: event.y,
                    client_x: event.x,
                    client_y: event.y,
                },
                buttons: MouseEventButtons::default(),
                mods: modifiers,
                element: Point::default(),
            }),
        ));
        // Native wheel default actions mutate Blitz scroll offsets without a
        // script mutation, so explicitly schedule the resulting repaint.
        self.dirty = true;
        self.apply_script_mutations(runtime);
        claimed.get()
    }

    pub fn ime_event(&mut self, runtime: &mut ShellRuntime, event: BlitzImeEvent) {
        let handler = DenoEventHandler {
            runtime,
            dom_to_script: &self.dom_to_script,
            claimed: Rc::new(Cell::new(false)),
            ordinary_dom_hit: false,
            target_override: None,
            default_prevented: None,
        };
        EventDriver::new(&mut self.document, handler).handle_ui_event(UiEvent::Ime(event));
        self.apply_script_mutations(runtime);
    }

    /// Dispatches a regular keyboard event to the shell window.
    pub fn keyboard_event(
        &mut self,
        runtime: &mut ShellRuntime,
        event: DomKeyboardEvent<'_>,
    ) -> bool {
        let mut modifiers = keyboard_types::Modifiers::empty();
        modifiers.set(keyboard_types::Modifiers::ALT, event.alt_key);
        modifiers.set(keyboard_types::Modifiers::CONTROL, event.ctrl_key);
        modifiers.set(keyboard_types::Modifiers::SHIFT, event.shift_key);
        let key = keyboard_types::Key::from_str(&event.key)
            .unwrap_or_else(|_| keyboard_types::Key::Character(event.key.clone()));
        let code = keyboard_types::Code::from_str(&event.code)
            .unwrap_or(keyboard_types::Code::Unidentified);
        let data = BlitzKeyEvent {
            key,
            code,
            modifiers,
            location: keyboard_types::Location::Standard,
            is_auto_repeating: event.repeat,
            is_composing: false,
            state: if event.kind == "keydown" {
                KeyState::Pressed
            } else {
                KeyState::Released
            },
            text: (event.kind == "keydown" && event.key.chars().count() == 1)
                .then(|| event.key.clone().into()),
        };
        let claimed = Rc::new(Cell::new(false));
        let handler = DenoEventHandler {
            runtime,
            dom_to_script: &self.dom_to_script,
            claimed: claimed.clone(),
            ordinary_dom_hit: false,
            target_override: None,
            default_prevented: None,
        };
        EventDriver::new(&mut self.document, handler).handle_ui_event(if event.kind == "keydown" {
            UiEvent::KeyDown(data)
        } else {
            UiEvent::KeyUp(data)
        });
        self.apply_script_mutations(runtime);
        claimed.get()
    }
}
