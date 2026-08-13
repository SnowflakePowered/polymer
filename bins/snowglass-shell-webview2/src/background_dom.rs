//! Owns the background document lifecycle and immutable DOM snapshots.
//!
//! `ShellCoordinator` decides what shell-wide effects a background request has, while
//! this type pairs an otherwise runtime-free document with its Deno driver.
//! Cross-realm transport is handled by the shared bus installed in that realm.
//! Emulator commands bypass it and enter the shell through the event loop.

use crate::background_document::{
    BackgroundDocument, BackgroundDocumentFactory, DomKeyboardEvent, DomMouseEvent, GameViewStyle,
};
use crate::background_runtime::BackgroundRuntime;
use kurbo::Rect;
use serde_json::Value;
use std::future::Future;

/// Paint output published by Blitz without exposing its document or renderer.
#[derive(Clone)]
pub(crate) struct BackgroundFrame {
    pub chrome_bands: Vec<vello::Scene>,
    pub width: u32,
    pub height: u32,
    pub dirty: bool,
}

/// Immutable compositor-facing layout published by the background DOM.
#[derive(Clone, Debug, PartialEq)]
pub(crate) struct TrackedSurfaceLayout {
    pub node_id: usize,
    pub source_id: u64,
    pub bounds: Rect,
    pub clipping: Option<Rect>,
    pub z_order: usize,
    pub corner_radius: f32,
    pub style: Option<GameViewStyle>,
    pub opacity: f32,
}

/// Result of querying the immutable layout used for an input-routing turn.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(crate) struct BackgroundHitSnapshot {
    pub node_id: usize,
    pub over_tracked_surface: bool,
}

pub(crate) struct BackgroundDomCoordinator {
    factory: BackgroundDocumentFactory,
    slot: BackgroundDocumentSlot,
}

enum BackgroundDocumentSlot {
    Awaiting(BackgroundDocumentFactory),
    Active(ActiveBackgroundDocument),
    Disabled,
}

struct ActiveBackgroundDocument {
    document: BackgroundDocument,
    runtime: BackgroundRuntime,
}

impl BackgroundDomCoordinator {
    pub fn new(factory: BackgroundDocumentFactory) -> Self {
        Self {
            slot: BackgroundDocumentSlot::Awaiting(factory.clone()),
            factory,
        }
    }

    pub fn reset(&mut self) {
        self.slot = BackgroundDocumentSlot::Awaiting(self.factory.clone());
    }

    fn active(&self) -> Option<&BackgroundDocument> {
        match &self.slot {
            BackgroundDocumentSlot::Active(active) => Some(&active.document),
            BackgroundDocumentSlot::Awaiting(_) | BackgroundDocumentSlot::Disabled => None,
        }
    }

    fn active_mut(&mut self) -> Option<&mut BackgroundDocument> {
        match &mut self.slot {
            BackgroundDocumentSlot::Active(active) => Some(&mut active.document),
            BackgroundDocumentSlot::Awaiting(_) | BackgroundDocumentSlot::Disabled => None,
        }
    }

    pub fn is_active(&self) -> bool {
        matches!(self.slot, BackgroundDocumentSlot::Active(_))
    }

    pub fn needs_paint(&self) -> bool {
        self.active().is_some_and(BackgroundDocument::needs_paint)
    }

    pub fn invalidate(&mut self) {
        if let Some(document) = self.active_mut() {
            document.invalidate();
        }
    }

    pub fn invalid_attachment(&self, plan: &crate::launch::AttachedLaunchPlan) -> Option<u32> {
        let document = self.active()?;
        plan.surfaces
            .iter()
            .find(|surface| !document.validates_attachment(surface.attach, surface.index))
            .map(|surface| surface.index)
    }

    pub async fn activate<F, Fut>(&mut self, create: F) -> bool
    where
        F: FnOnce(BackgroundDocumentFactory) -> Fut,
        Fut: Future<Output = (BackgroundDocument, BackgroundRuntime)>,
    {
        let slot = std::mem::replace(&mut self.slot, BackgroundDocumentSlot::Disabled);
        match slot {
            BackgroundDocumentSlot::Awaiting(factory) => {
                let (document, runtime) = create(factory).await;
                self.slot =
                    BackgroundDocumentSlot::Active(ActiveBackgroundDocument { document, runtime });
                true
            }
            slot => {
                self.slot = slot;
                false
            }
        }
    }

    pub fn disable(&mut self) {
        if matches!(self.slot, BackgroundDocumentSlot::Awaiting(_)) {
            self.slot = BackgroundDocumentSlot::Disabled;
        }
    }

    pub fn tracked_surface_layout(&self) -> Vec<TrackedSurfaceLayout> {
        let Some(layer) = self.active() else {
            return Vec::new();
        };
        layer
            .placements()
            .iter()
            .filter_map(|placement| {
                Some(TrackedSurfaceLayout {
                    node_id: placement.node_id?,
                    source_id: placement.source_id,
                    bounds: placement.rect,
                    clipping: placement.clipping,
                    z_order: placement.band,
                    corner_radius: placement.corner_radius,
                    style: placement.style.clone(),
                    opacity: placement.opacity,
                })
            })
            .collect()
    }

    pub fn hit_test(&self, x: f32, y: f32) -> Option<BackgroundHitSnapshot> {
        self.active()?.hit_test(x, y)
    }

    pub fn produce_frame(&mut self) -> Option<BackgroundFrame> {
        let BackgroundDocumentSlot::Active(active) = &mut self.slot else {
            return None;
        };
        Some(active.document.produce_frame(&mut active.runtime))
    }

    pub fn attach_shared_bus(&mut self, endpoint: crate::shared_bus::BackgroundBusEndpoint) {
        self.factory.attach_shared_bus(endpoint.clone());
        if let BackgroundDocumentSlot::Awaiting(factory) = &mut self.slot {
            factory.attach_shared_bus(endpoint);
        }
    }

    pub fn post_message(&mut self, message: &Value) -> bool {
        let BackgroundDocumentSlot::Active(active) = &mut self.slot else {
            return false;
        };
        active.runtime.post_message(message);
        active.document.apply_script_mutations(&mut active.runtime);
        true
    }

    pub async fn poll_runtime(&mut self) -> bool {
        let BackgroundDocumentSlot::Active(active) = &mut self.slot else {
            return false;
        };
        let polled = active.runtime.poll_event_loop().await;
        active.document.apply_script_mutations(&mut active.runtime);
        polled
    }

    pub fn set_input_configuration(&mut self, configuration: &Value, layouts: &Value) {
        if let BackgroundDocumentSlot::Active(active) = &mut self.slot {
            active
                .runtime
                .set_input_configuration(configuration, layouts);
        }
    }

    pub fn input_configuration_result(&mut self, request_id: u64, result: &Value) {
        if let BackgroundDocumentSlot::Active(active) = &mut self.slot {
            active
                .runtime
                .input_configuration_result(request_id, result);
        }
    }

    pub fn set_input_state(&mut self, controllers: &Value, priority: &str) {
        if let BackgroundDocumentSlot::Active(active) = &mut self.slot {
            active.runtime.set_input_state(controllers, priority);
        }
    }

    pub fn resize(&mut self, width: u32, height: u32) -> bool {
        let BackgroundDocumentSlot::Active(active) = &mut self.slot else {
            return false;
        };
        active.document.resize(width, height);
        active.runtime.resize(width, height);
        active.document.apply_script_mutations(&mut active.runtime);
        true
    }

    pub fn mouse_event(&mut self, hit: BackgroundHitSnapshot, event: DomMouseEvent<'_>) -> bool {
        let BackgroundDocumentSlot::Active(active) = &mut self.slot else {
            return false;
        };
        active.document.mouse_event(&mut active.runtime, hit, event)
    }

    pub fn keyboard_event(&mut self, event: DomKeyboardEvent<'_>) -> bool {
        let BackgroundDocumentSlot::Active(active) = &mut self.slot else {
            return false;
        };
        active.document.keyboard_event(&mut active.runtime, event)
    }

    pub fn dispatch_emulator_event(&mut self, event: &Value) -> bool {
        let BackgroundDocumentSlot::Active(active) = &mut self.slot else {
            return false;
        };
        active.runtime.dispatch_emulator_event(event);
        active.document.apply_script_mutations(&mut active.runtime);
        active.document.needs_paint()
    }
}
