//! Owns the shell document lifecycle and immutable DOM snapshots.
//!
//! `ShellCoordinator` decides shell-wide effects while this type pairs an
//! otherwise runtime-free document with its same-realm Deno driver. Native
//! emulator commands enter the shell through the event loop.

use crate::shell_document::{
    CompositionShadowStyle, DomKeyboardEvent, DomMouseEvent, DomWheelEvent, GameViewStyle,
    ShellDocument, ShellDocumentFactory,
};
use crate::shell_runtime::{ShellRuntime, ShellRuntimeOptions};
use kurbo::Rect;
use std::{
    cell::{Ref, RefCell, RefMut},
    future::Future,
    rc::{Rc, Weak},
};

/// Paint output published by Blitz without exposing its document or renderer.
#[derive(Clone)]
pub(crate) struct ShellFrame {
    pub raster_chunks: Vec<ShellRasterChunk>,
    pub composition_groups: Vec<ShellCompositionGroup>,
    pub surface_layout: Vec<TrackedSurfaceLayout>,
    pub width: u32,
    pub height: u32,
}

#[derive(Clone)]
pub(crate) struct ShellRasterChunk {
    pub id: RasterChunkId,
    pub scene: anyrender::Scene,
    pub bounds: Rect,
    pub z_order: i32,
    pub group: Option<u64>,
}

/// Stable identity for one raster leaf within a retained compositor owner.
/// Root chrome and each promoted stacking context have independent ordinal
/// spaces, so unrelated group insertion does not churn existing WUC visuals.
#[derive(Clone, Copy, Debug, Eq, Hash, PartialEq)]
pub(crate) struct RasterChunkId {
    pub owner: Option<u64>,
    pub ordinal: u32,
}

#[derive(Clone, Debug, PartialEq)]
pub(crate) struct RasterChunkLayout {
    pub id: RasterChunkId,
    pub bounds: Rect,
    pub z_order: i32,
    pub group: Option<u64>,
}

#[derive(Clone, Debug, PartialEq)]
pub(crate) struct ShellCompositionGroup {
    pub id: u64,
    pub parent: Option<u64>,
    pub bounds: Rect,
    pub z_order: i32,
    pub opacity: f32,
    pub shadow: Option<CompositionShadowStyle>,
}

/// Immutable compositor-facing layout published by the shell DOM.
#[derive(Clone, Debug, PartialEq)]
pub(crate) struct TrackedSurfaceLayout {
    pub node_id: usize,
    pub source_id: u64,
    pub bounds: Rect,
    pub z_order: i32,
    pub group: Option<u64>,
    pub corner_radius: f32,
    pub style: Option<GameViewStyle>,
    pub opacity: f32,
}

/// Result of querying the immutable layout used for an input-routing turn.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(crate) struct ShellHitSnapshot {
    pub node_id: blitz_dom::NodeId,
    pub over_tracked_surface: bool,
}

pub(crate) struct ShellSession {
    slot: ShellDocumentSlot,
}

enum ShellDocumentSlot {
    Awaiting(ShellSessionFactory),
    Active(Box<ActiveShellDocument>),
    Disabled,
}

struct ActiveShellDocument {
    document: Rc<RefCell<ShellDocument>>,
    runtime: ShellRuntime,
}

impl ShellSession {
    pub fn new(factory: ShellSessionFactory) -> Self {
        Self {
            slot: ShellDocumentSlot::Awaiting(factory),
        }
    }

    fn active(&self) -> Option<Ref<'_, ShellDocument>> {
        match &self.slot {
            ShellDocumentSlot::Active(active) => Some(active.document.borrow()),
            ShellDocumentSlot::Awaiting(_) | ShellDocumentSlot::Disabled => None,
        }
    }

    fn active_mut(&mut self) -> Option<RefMut<'_, ShellDocument>> {
        match &mut self.slot {
            ShellDocumentSlot::Active(active) => Some(active.document.borrow_mut()),
            ShellDocumentSlot::Awaiting(_) | ShellDocumentSlot::Disabled => None,
        }
    }

    pub fn is_active(&self) -> bool {
        matches!(self.slot, ShellDocumentSlot::Active(_))
    }

    pub(crate) fn inspector_document(&self) -> Weak<RefCell<ShellDocument>> {
        match &self.slot {
            ShellDocumentSlot::Active(active) => Rc::downgrade(&active.document),
            ShellDocumentSlot::Awaiting(_) | ShellDocumentSlot::Disabled => Weak::new(),
        }
    }

    pub(crate) fn inspector_address(&self) -> Option<std::net::SocketAddr> {
        match &self.slot {
            ShellDocumentSlot::Active(active) => Some(active.runtime.inspector_address()),
            ShellDocumentSlot::Awaiting(_) | ShellDocumentSlot::Disabled => None,
        }
    }

    pub(crate) fn inspector_structure_revision(&self) -> Option<u64> {
        self.active()
            .map(|document| document.dom_structure_revision())
    }

    pub fn needs_paint(&self) -> bool {
        self.active().is_some_and(|document| document.needs_paint())
    }

    pub fn invalidate(&mut self) {
        if let Some(mut document) = self.active_mut() {
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
            active
                .document
                .borrow_mut()
                .apply_script_mutations(&mut active.runtime);
        }
    }

    pub async fn activate<F, Fut>(
        &mut self,
        create: F,
    ) -> Result<Option<crate::shell_runtime::PolymerNotificationSender>, Box<dyn std::error::Error>>
    where
        F: FnOnce(ShellSessionFactory) -> Fut,
        Fut: Future<
            Output = Result<
                (
                    ShellDocument,
                    ShellRuntime,
                    crate::shell_runtime::PolymerNotificationSender,
                ),
                Box<dyn std::error::Error>,
            >,
        >,
    {
        let slot = std::mem::replace(&mut self.slot, ShellDocumentSlot::Disabled);
        match slot {
            ShellDocumentSlot::Awaiting(factory) => {
                let (document, runtime, notifications) = create(factory).await?;
                self.slot = ShellDocumentSlot::Active(Box::new(ActiveShellDocument {
                    document: Rc::new(RefCell::new(document)),
                    runtime,
                }));
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

    pub fn hit_test(&self, x: f32, y: f32) -> Option<ShellHitSnapshot> {
        self.active()?.hit_test(x, y)
    }

    pub fn produce_frame(&mut self) -> Option<ShellFrame> {
        let ShellDocumentSlot::Active(active) = &mut self.slot else {
            return None;
        };
        active
            .document
            .borrow_mut()
            .produce_frame(&mut active.runtime)
    }

    pub async fn poll_runtime(&mut self) -> bool {
        let ShellDocumentSlot::Active(active) = &mut self.slot else {
            return false;
        };
        active.runtime.poll_event_loop().await;
        active
            .document
            .borrow_mut()
            .apply_script_mutations(&mut active.runtime);
        true
    }

    pub fn resize(&mut self, width: u32, height: u32, scale_factor: f32) -> bool {
        let ShellDocumentSlot::Active(active) = &mut self.slot else {
            return false;
        };
        let mut document = active.document.borrow_mut();
        document.resize(width, height, scale_factor);
        let (logical_width, logical_height) = document.logical_size();
        active
            .runtime
            .resize(logical_width, logical_height, document.scale_factor());
        document.apply_script_mutations(&mut active.runtime);
        true
    }

    pub fn physical_to_css(&self, x: f32, y: f32) -> (f32, f32) {
        self.active()
            .map_or((x, y), |document| document.physical_to_css(x, y))
    }

    pub fn mouse_event(&mut self, hit: ShellHitSnapshot, event: DomMouseEvent<'_>) -> bool {
        let ShellDocumentSlot::Active(active) = &mut self.slot else {
            return false;
        };
        active
            .document
            .borrow_mut()
            .mouse_event(&mut active.runtime, hit, event)
    }

    pub fn clear_pointer_hover(&mut self) -> bool {
        let ShellDocumentSlot::Active(active) = &mut self.slot else {
            return false;
        };
        let mut document = active.document.borrow_mut();
        document.clear_pointer_hover();
        document.needs_paint()
    }

    pub fn keyboard_event(&mut self, event: DomKeyboardEvent<'_>) -> bool {
        let ShellDocumentSlot::Active(active) = &mut self.slot else {
            return false;
        };
        active
            .document
            .borrow_mut()
            .keyboard_event(&mut active.runtime, event)
    }

    pub fn wheel_event(&mut self, hit: ShellHitSnapshot, event: DomWheelEvent) -> bool {
        let ShellDocumentSlot::Active(active) = &mut self.slot else {
            return false;
        };
        active
            .document
            .borrow_mut()
            .wheel_event(&mut active.runtime, hit, event)
    }

    pub fn ime_event(&mut self, event: blitz_traits::events::BlitzImeEvent) {
        if let ShellDocumentSlot::Active(active) = &mut self.slot {
            active
                .document
                .borrow_mut()
                .ime_event(&mut active.runtime, event);
        }
    }
}

pub(crate) struct ShellSessionFactory {
    document: ShellDocumentFactory,
    plugin_runtime: std::sync::Arc<polymer_pluginhost::runtime::PluginRuntime>,
    local_storage: Option<polymer_deno::webstorage::LocalStorage>,
    library: Option<polymer_runtime::LibraryService>,
    input_sources: polymer_input::InputHub,
    controller_api: polymer_runtime::ControllerApiState,
}

impl ShellSessionFactory {
    pub(crate) fn new(
        plugin_registry: polymer_pluginhost::loading::PluginRegistry,
        directories: polymer_library::Directories,
        local_storage: Option<polymer_deno::webstorage::LocalStorage>,
        library: Option<polymer_runtime::LibraryService>,
        event_proxy: crate::window_manager::EventLoopProxy<crate::app::UserEvent>,
    ) -> Self {
        let plugin_runtime = polymer_pluginhost::runtime::PluginRuntime::new(
            plugin_registry,
            directories,
            polymer_pluginhost::runtime::PluginRuntimeOptions::default(),
        );
        Self {
            document: ShellDocumentFactory::new(event_proxy),
            plugin_runtime: std::sync::Arc::new(plugin_runtime),
            local_storage,
            library,
            input_sources: polymer_input::InputHub::default(),
            controller_api: polymer_runtime::ControllerApiState::default(),
        }
    }

    pub(crate) fn input_sources(&self) -> polymer_input::InputHub {
        self.input_sources.clone()
    }

    pub(crate) fn controller_api(&self) -> polymer_runtime::ControllerApiState {
        self.controller_api.clone()
    }

    #[allow(clippy::too_many_arguments)]
    pub(crate) async fn create(
        self,
        html: &str,
        base_url: String,
        width: u32,
        height: u32,
        scale_factor: f32,
        commands: polymer_runtime::FrontendCommands,
    ) -> Result<
        (
            ShellDocument,
            ShellRuntime,
            crate::shell_runtime::PolymerNotificationSender,
        ),
        Box<dyn std::error::Error>,
    > {
        let mut document =
            self.document
                .create(html, base_url.clone(), width, height, scale_factor);
        let (base_document, width, height, scale_factor) = document.runtime_context();
        let (mut runtime, notifications) = ShellRuntime::start(
            base_document,
            width,
            height,
            scale_factor,
            document.runtime_node_ids(),
            ShellRuntimeOptions {
                html: html.to_owned(),
                source_name: base_url,
                commands,
                plugin_runtime: self.plugin_runtime,
                local_storage: self.local_storage,
                library: self.library,
                input_sources: self.input_sources,
                controller_api: self.controller_api,
            },
        )
        .await?;
        document.apply_script_mutations(&mut runtime);
        Ok((document, runtime, notifications))
    }
}
