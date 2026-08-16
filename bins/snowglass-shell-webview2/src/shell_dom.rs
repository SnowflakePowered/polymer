//! Owns the shell document lifecycle and immutable DOM snapshots.
//!
//! `ShellCoordinator` decides shell-wide effects while this type pairs an
//! otherwise runtime-free document with its same-realm Deno driver. Native
//! emulator commands enter the shell through the event loop.

use crate::shell_document::{
    DomKeyboardEvent, DomMouseEvent, DomWheelEvent, GameViewStyle, ShellDocument,
    ShellDocumentFactory,
};
use crate::shell_runtime::ShellRuntime;
use kurbo::Rect;
use std::future::Future;

/// Paint output published by Blitz without exposing its document or renderer.
#[derive(Clone)]
pub(crate) struct ShellFrame {
    pub chrome_bands: Vec<vello::Scene>,
    pub width: u32,
    pub height: u32,
    pub dirty: bool,
    /// A mutation inside a CSS paint/layout containment boundary can redraw
    /// only that boundary's old and new paint bounds. `None` means the whole
    /// frame changed.
    pub damage: Option<kurbo::Rect>,
    /// Optional renderer-level band restriction. Contained DOM damage is sent
    /// to every band because visual overflow can cross tracked-surface splits.
    pub dirty_bands: Option<Vec<usize>>,
}

/// Immutable compositor-facing layout published by the shell DOM.
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
pub(crate) struct ShellHitSnapshot {
    pub node_id: usize,
    pub over_tracked_surface: bool,
}

pub(crate) struct ShellDomCoordinator {
    slot: ShellDocumentSlot,
}

enum ShellDocumentSlot {
    Awaiting(ShellDocumentFactory),
    Active(ActiveShellDocument),
    Disabled,
}

struct ActiveShellDocument {
    document: ShellDocument,
    runtime: ShellRuntime,
}

impl ShellDomCoordinator {
    pub fn new(factory: ShellDocumentFactory) -> Self {
        Self {
            slot: ShellDocumentSlot::Awaiting(factory),
        }
    }

    fn active(&self) -> Option<&ShellDocument> {
        match &self.slot {
            ShellDocumentSlot::Active(active) => Some(&active.document),
            ShellDocumentSlot::Awaiting(_) | ShellDocumentSlot::Disabled => None,
        }
    }

    fn active_mut(&mut self) -> Option<&mut ShellDocument> {
        match &mut self.slot {
            ShellDocumentSlot::Active(active) => Some(&mut active.document),
            ShellDocumentSlot::Awaiting(_) | ShellDocumentSlot::Disabled => None,
        }
    }

    pub fn is_active(&self) -> bool {
        matches!(self.slot, ShellDocumentSlot::Active(_))
    }

    pub fn needs_paint(&self) -> bool {
        self.active().is_some_and(ShellDocument::needs_paint)
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

    /// Applies mutations queued by the UI isolate before a synchronous
    /// runtime command consumes DOM node references from that same turn.
    pub fn flush_script_mutations(&mut self) {
        if let ShellDocumentSlot::Active(active) = &mut self.slot {
            active.document.apply_script_mutations(&mut active.runtime);
        }
    }

    pub async fn activate<F, Fut>(
        &mut self,
        create: F,
    ) -> Result<Option<crate::shell_runtime::SnowglassNotificationSender>, Box<dyn std::error::Error>>
    where
        F: FnOnce(ShellDocumentFactory) -> Fut,
        Fut: Future<
            Output = Result<
                (
                    ShellDocument,
                    ShellRuntime,
                    crate::shell_runtime::SnowglassNotificationSender,
                ),
                Box<dyn std::error::Error>,
            >,
        >,
    {
        let slot = std::mem::replace(&mut self.slot, ShellDocumentSlot::Disabled);
        match slot {
            ShellDocumentSlot::Awaiting(factory) => {
                let (document, runtime, notifications) = create(factory).await?;
                self.slot = ShellDocumentSlot::Active(ActiveShellDocument { document, runtime });
                Ok(Some(notifications))
            }
            slot => {
                self.slot = slot;
                Ok(None)
            }
        }
    }

    pub fn disable(&mut self) {
        if matches!(self.slot, ShellDocumentSlot::Awaiting(_)) {
            self.slot = ShellDocumentSlot::Disabled;
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

    pub fn hit_test(&self, x: f32, y: f32) -> Option<ShellHitSnapshot> {
        self.active()?.hit_test(x, y)
    }

    pub fn produce_frame(&mut self) -> Option<ShellFrame> {
        let ShellDocumentSlot::Active(active) = &mut self.slot else {
            return None;
        };
        Some(active.document.produce_frame(&mut active.runtime))
    }

    pub async fn poll_runtime(&mut self) -> bool {
        let ShellDocumentSlot::Active(active) = &mut self.slot else {
            return false;
        };
        let polled = active.runtime.poll_event_loop().await;
        active.document.apply_script_mutations(&mut active.runtime);
        polled
    }

    pub fn resize(&mut self, width: u32, height: u32) -> bool {
        let ShellDocumentSlot::Active(active) = &mut self.slot else {
            return false;
        };
        active.document.resize(width, height);
        active.runtime.resize(width, height);
        active.document.apply_script_mutations(&mut active.runtime);
        true
    }

    pub fn mouse_event(&mut self, hit: ShellHitSnapshot, event: DomMouseEvent<'_>) -> bool {
        let ShellDocumentSlot::Active(active) = &mut self.slot else {
            return false;
        };
        active.document.mouse_event(&mut active.runtime, hit, event)
    }

    pub fn clear_pointer_hover(&mut self) -> bool {
        let ShellDocumentSlot::Active(active) = &mut self.slot else {
            return false;
        };
        active.document.clear_pointer_hover();
        active.document.needs_paint()
    }

    pub fn keyboard_event(&mut self, event: DomKeyboardEvent<'_>) -> bool {
        let ShellDocumentSlot::Active(active) = &mut self.slot else {
            return false;
        };
        active.document.keyboard_event(&mut active.runtime, event)
    }

    pub fn wheel_event(&mut self, hit: ShellHitSnapshot, event: DomWheelEvent) -> bool {
        let ShellDocumentSlot::Active(active) = &mut self.slot else {
            return false;
        };
        active.document.wheel_event(&mut active.runtime, hit, event)
    }

    pub fn ime_event(&mut self, event: blitz_traits::events::BlitzImeEvent) {
        if let ShellDocumentSlot::Active(active) = &mut self.slot {
            active.document.ime_event(&mut active.runtime, event);
        }
    }
}
