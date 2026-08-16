//! Lays out and paints the sole shell document with Blitz.
//!
//! Ordinary Solid UI, bezel art, and guest-frame placeholders all live in
//! this document and are composited without a browser process.
//!
//! A game view is written as `<tracked-surface src="N">`, where N is the
//! tracked render target. Painting diverts those into rectangles
//! ([`bands::GamePlacement`])
//! and splits the remaining chrome into bands, which the compositor stacks
//! around them.

mod bands;

use crate::shell_dom::{ShellFrame, ShellHitSnapshot};
use crate::shell_runtime::{EmulatorCommandSink, LauncherCommandSink, Mutation, ShellRuntime};
use anyrender::PaintScene as _;
use anyrender_vello::VelloScenePainter;
use bands::BandedScene;
pub use bands::{GamePlacement, GameViewStyle};
use blitz_dom::node::{Node, Widget};
use blitz_dom::{EventDriver, EventHandler, LocalName, QualName, local_name, ns};
use blitz_traits::events::{
    BlitzImeEvent, BlitzKeyEvent, BlitzPointerEvent, BlitzPointerId, BlitzWheelDelta,
    BlitzWheelEvent, DomEvent, DomEventData, EventState, KeyState, MouseEventButton,
    MouseEventButtons, Point, PointerCoords, PointerDetails, UiEvent,
};
use blitz_traits::net::{Bytes, NetHandler, NetProvider, NetWaker, Request};
use blitz_traits::shell::{ClipboardError, ShellProvider};
use std::{
    cell::Cell,
    collections::HashMap,
    rc::Rc,
    str::FromStr,
    sync::{
        Arc, Mutex,
        mpsc::{Receiver, channel},
    },
};
use style::values::{
    computed::{Overflow, PointerEvents},
    specified::box_::Contain,
};
use vello::Scene;

/// How many times a paint will re-resolve to pick up assets that only became
/// reachable through an asset it just loaded.
const ASSET_ROUNDS: usize = 4;

/// Set to report what each repaint of this layer costs.
const PAINT_TIMING: &str = "SNOWGLASS_GAME_LAYER_TIMING";

struct NativeShellProvider {
    proxy: crate::window_manager::EventLoopProxy<crate::app::UserEvent>,
    clipboard: Mutex<Option<arboard::Clipboard>>,
}

/// Delegates normal resource loading to `blitz-net` while correcting its
/// Windows file-URL conversion. `Url::path()` produces `/D:/...`, which the
/// Windows filesystem interprets as `D:\\D:\\...`; `to_file_path()` produces
/// the native path that linked built assets actually occupy.
struct ShellNetProvider {
    upstream: Arc<dyn NetProvider>,
    waker: Arc<dyn NetWaker>,
}

impl NetProvider for ShellNetProvider {
    fn fetch(&self, doc_id: usize, request: Request, handler: Box<dyn NetHandler>) {
        #[cfg(windows)]
        if request.url.scheme() == "file" {
            let url = request.url;
            let waker = self.waker.clone();
            std::thread::spawn(move || {
                match url.to_file_path() {
                    Ok(path) => match std::fs::read(&path) {
                        Ok(contents) => {
                            handler.bytes(url.to_string(), Bytes::from(contents));
                        }
                        Err(error) => tracing::error!(
                            path = %path.display(),
                            "snowglass: loading shell file asset failed: {error}"
                        ),
                    },
                    Err(()) => tracing::error!(
                        url = %url,
                        "snowglass: converting shell file URL failed"
                    ),
                }
                // Wake only after the handler has installed the resource. The
                // upstream provider wakes before its callback, which can race
                // the shell's next resolve on fast local files.
                waker.wake(doc_id);
            });
            return;
        }

        self.upstream.fetch(doc_id, request, handler);
    }
}

impl NativeShellProvider {
    fn new(proxy: crate::window_manager::EventLoopProxy<crate::app::UserEvent>) -> Self {
        Self {
            proxy,
            clipboard: Mutex::new(arboard::Clipboard::new().ok()),
        }
    }

    fn send(&self, command: crate::window_manager::WindowCommand) {
        let _ = self
            .proxy
            .send_event(crate::commands::UserEvent::Window(command));
    }
}

impl ShellProvider for NativeShellProvider {
    fn request_redraw(&self) {
        self.send(crate::window_manager::WindowCommand::RedrawShell);
    }

    fn set_cursor(&self, icon: Option<cursor_icon::CursorIcon>) {
        self.send(crate::window_manager::WindowCommand::SetCursor(icon));
    }

    fn set_window_title(&self, title: String) {
        self.send(crate::window_manager::WindowCommand::SetTitle(title));
    }

    fn set_ime_enabled(&self, is_enabled: bool) {
        self.send(crate::window_manager::WindowCommand::SetImeEnabled(
            is_enabled,
        ));
    }

    fn set_ime_cursor_area(&self, x: f32, y: f32, width: f32, height: f32) {
        self.send(crate::window_manager::WindowCommand::SetImeCursorArea {
            x,
            y,
            width,
            height,
        });
    }

    fn get_clipboard_text(&self) -> Result<String, ClipboardError> {
        self.clipboard
            .lock()
            .map_err(|_| ClipboardError)?
            .as_mut()
            .ok_or(ClipboardError)?
            .get_text()
            .map_err(|_| ClipboardError)
    }

    fn set_clipboard_text(&self, text: String) -> Result<(), ClipboardError> {
        self.clipboard
            .lock()
            .map_err(|_| ClipboardError)?
            .as_mut()
            .ok_or(ClipboardError)?
            .set_text(text)
            .map_err(|_| ClipboardError)
    }
}

/// Hit-tests in the same stacking order Blitz paints while allowing a child
/// to restore `pointer-events: auto` beneath a transparent parent.
///
/// Blitz 0.3 hoists non-zero z-index descendants out of `paint_children`.
/// Those lists therefore have to be traversed explicitly around the ordinary
/// children, just as the painter does.
fn pointer_hit(document: &blitz_dom::BaseDocument, x: f32, y: f32) -> Option<usize> {
    // Paint children include anonymous/text layout nodes. Browser events target
    // their nearest element, and Deno only has stable wrappers for DOM nodes;
    // dispatching an anonymous paint node otherwise collapses to `document`.
    let painted = pointer_hit_node(document, document.root_element(), x, y).and_then(|node| {
        std::iter::successors(Some(node), |node| {
            document.get_node(*node).and_then(|node| node.parent)
        })
        .find(|node| {
            document
                .get_node(*node)
                .and_then(blitz_dom::Node::element_data)
                .is_some()
        })
    });
    let root = document.root_element().id;
    // The paint traversal is the only result that accounts for stacking and
    // hoisted z-index descendants.  Keep it whenever it found ordinary UI;
    // the geometry walk below is only a one-frame fallback for a freshly
    // inserted control that is painted over a tracked surface but has not yet
    // reached Blitz's cached paint tree.
    if painted.is_some_and(|node| {
        node != root
            && !node_is_within_tracked_surface(document, node)
            && painted_pointer_target_is_valid(document, node, x, y)
    }) {
        return painted;
    }
    let mut geometry_hit = None;
    for (order, (node_id, node)) in document.tree().iter().enumerate() {
        let accepts_pointer = node
            .primary_styles()
            .is_none_or(|styles| styles.clone_pointer_events() != PointerEvents::None);
        if !accepts_pointer {
            continue;
        }
        let Some(rect) = document.get_client_bounding_rect(node_id) else {
            continue;
        };
        if rect.width <= 0.0
            || rect.height <= 0.0
            || f64::from(x) < rect.x
            || f64::from(x) > rect.x + rect.width
            || f64::from(y) < rect.y
            || f64::from(y) > rect.y + rect.height
        {
            continue;
        }
        let mut depth = 0usize;
        let mut ancestor = node_id;
        while ancestor != root {
            let Some(parent) = document.get_node(ancestor).and_then(|node| node.parent) else {
                depth = 0;
                break;
            };
            depth += 1;
            ancestor = parent;
        }
        if ancestor != root {
            continue;
        }
        // Anonymous text boxes and even stylesheet nodes can acquire geometry
        // in Blitz's tree. Never let those replace a valid paint hit: the
        // geometry fallback exists specifically to recover an interactive DOM
        // ancestor for newly inserted controls whose cached paint hit tree is
        // stale for one frame.
        let Some(interactive) = std::iter::successors(Some(node_id), |node| {
            document.get_node(*node).and_then(|node| node.parent)
        })
        .find(|node| {
            document
                .get_node(*node)
                .and_then(blitz_dom::Node::element_data)
                .is_some_and(|element| {
                    matches!(
                        element.name.local.as_ref(),
                        "button" | "input" | "textarea" | "select" | "a" | "label"
                    ) || matches!(element.attr(local_name!("role")), Some("button" | "link"))
                })
        }) else {
            continue;
        };
        if geometry_hit
            .as_ref()
            .is_none_or(|(best_depth, best_order, _)| (depth, order) > (*best_depth, *best_order))
        {
            geometry_hit = Some((depth, order, interactive));
        }
    }
    geometry_hit.map(|(_, _, node)| node).or(painted)
}

/// Rejects paint-tree sentinels and scroll containers whose cached overflow
/// extends beyond their actual client box. This stays deliberately O(1): the
/// recursive paint walk has already applied ancestor clips and transforms, so
/// walking every ancestor's geometry here both duplicates work and makes
/// pointer-driven transforms unnecessarily expensive.
fn painted_pointer_target_is_valid(
    document: &blitz_dom::BaseDocument,
    node_id: usize,
    x: f32,
    y: f32,
) -> bool {
    let Some(node) = document.get_node(node_id) else {
        return false;
    };
    if node.element_data().is_some_and(|element| {
        matches!(
            element.name.local.as_ref(),
            "head" | "style" | "script" | "link" | "meta" | "title"
        )
    }) {
        return false;
    }
    let clips_overflow = node.primary_styles().is_some_and(|styles| {
        let box_style = styles.get_box();
        !matches!(box_style.clone_overflow_x(), Overflow::Visible)
            || !matches!(box_style.clone_overflow_y(), Overflow::Visible)
    });
    if !clips_overflow || node.transform.is_some() {
        return true;
    }
    document.get_client_bounding_rect(node_id).is_some_and(|rect| {
        f64::from(x) >= rect.x
            && f64::from(x) <= rect.x + rect.width
            && f64::from(y) >= rect.y
            && f64::from(y) <= rect.y + rect.height
    })
}

fn node_is_within_tracked_surface(document: &blitz_dom::BaseDocument, node_id: usize) -> bool {
    std::iter::successors(Some(node_id), |node| {
        document.get_node(*node).and_then(|node| node.parent)
    })
    .any(|node| {
        document
            .get_node(node)
            .and_then(blitz_dom::Node::element_data)
            .is_some_and(is_tracked_surface_element)
    })
}

fn pointer_hit_node(
    document: &blitz_dom::BaseDocument,
    node: &Node,
    x: f32,
    y: f32,
) -> Option<usize> {
    let mut local_x = x - node.final_layout.location.x + node.scroll_offset.x as f32;
    let mut local_y = y - node.final_layout.location.y + node.scroll_offset.y as f32;
    if let Some(transform) = node.transform {
        let point = transform.inverse() * kurbo::Point::new(local_x.into(), local_y.into());
        local_x = point.x as f32;
        local_y = point.y as f32;
    }

    let size = node.final_layout.size;
    let matches_self = local_x >= 0.0
        && local_x <= size.width + node.scroll_offset.x as f32
        && local_y >= 0.0
        && local_y <= size.height + node.scroll_offset.y as f32;
    let content = node.final_layout.content_size;
    let matches_content = local_x >= 0.0
        && local_x <= content.width + node.scroll_offset.x as f32
        && local_y >= 0.0
        && local_y <= content.height + node.scroll_offset.y as f32;
    let overflow = node.scrollable_overflow;
    let matches_overflow = local_x >= overflow.x0 as f32
        && local_x <= overflow.x1 as f32
        && local_y >= overflow.y0 as f32
        && local_y <= overflow.y1 as f32;
    let matches_hoisted = node.stacking_context.as_ref().is_some_and(|context| {
        let area = context.content_area;
        local_x >= area.left + node.scroll_offset.x as f32
            && local_x <= area.right + node.scroll_offset.x as f32
            && local_y >= area.top + node.scroll_offset.y as f32
            && local_y <= area.bottom + node.scroll_offset.y as f32
    });
    if !matches_self && !matches_content && !matches_overflow && !matches_hoisted {
        return None;
    }
    let clips_overflow = node.primary_styles().is_some_and(|styles| {
        let box_style = styles.get_box();
        !matches!(box_style.clone_overflow_x(), Overflow::Visible)
            || !matches!(box_style.clone_overflow_y(), Overflow::Visible)
    });
    if clips_overflow && !matches_self {
        return None;
    }

    if let Some(context) = &node.stacking_context {
        for child in context.pos_z_hoisted_children().rev() {
            if let Some(hit) = document.get_node(child.node_id).and_then(|node| {
                pointer_hit_node(
                    document,
                    node,
                    local_x - child.position.x,
                    local_y - child.position.y,
                )
            }) {
                return Some(hit);
            }
        }
    }

    let children = node.paint_children.borrow().clone().unwrap_or_default();
    for child in children.into_iter().rev() {
        if let Some(hit) = document
            .get_node(child)
            .and_then(|child| pointer_hit_node(document, child, local_x, local_y))
        {
            return Some(hit);
        }
    }

    if let Some(context) = &node.stacking_context {
        for child in context.neg_z_hoisted_children().rev() {
            if let Some(hit) = document.get_node(child.node_id).and_then(|node| {
                pointer_hit_node(
                    document,
                    node,
                    local_x - child.position.x,
                    local_y - child.position.y,
                )
            }) {
                return Some(hit);
            }
        }
    }

    let accepts_pointer = node
        .primary_styles()
        .is_none_or(|styles| styles.clone_pointer_events() != PointerEvents::None);
    (matches_self && accepts_pointer).then_some(node.id)
}

#[derive(Default)]
struct TrackedSurfaceWidget {
    source_id: Option<u64>,
}

impl TrackedSurfaceWidget {
    fn new(source_id: Option<u64>) -> Self {
        Self { source_id }
    }
}

impl Widget for TrackedSurfaceWidget {
    fn attribute_changed(&mut self, name: &str, _old_value: Option<&str>, new_value: Option<&str>) {
        if name == "src" {
            self.source_id = new_value.and_then(|value| value.parse().ok());
        }
    }

    fn paint(
        &mut self,
        _render_ctx: &mut dyn anyrender::RenderContext,
        _styles: &blitz_dom::node::ComputedStyles,
        width: u32,
        height: u32,
        _scale: f64,
    ) -> anyrender::Scene {
        let mut scene = anyrender::Scene::new();
        if let Some(source_id) = self.source_id {
            let paint = anyrender::Paint::Resource(peniko::ImageBrush {
                image: anyrender::ResourceId::from_ffi(source_id),
                sampler: Default::default(),
            });
            scene.fill(
                peniko::Fill::NonZero,
                kurbo::Affine::IDENTITY,
                &paint,
                None,
                &kurbo::Rect::new(0.0, 0.0, f64::from(width), f64::from(height)),
            );
        }
        scene
    }
}

pub struct ShellDocument {
    document: blitz_html::HtmlDocument,
    /// Assets arrive here because a fetch is issued while the document is
    /// mid-resolve and cannot take them straight away.
    arrivals: Receiver<()>,
    /// One encoded scene per band. Encoding is cheap and happens on the CPU;
    /// only rasterising is worth a GPU.
    bands: Vec<Scene>,
    placements: Vec<GamePlacement>,
    width: u32,
    height: u32,
    /// Set whenever layout or content changed, so a paint is only spent when
    /// something can differ. Guest frames arriving do not dirty this layer.
    dirty: bool,
    /// Damage accumulated for mutations below a CSS layout/paint containment
    /// boundary. Any uncontained change sets `full_repaint` and discards it.
    partial_damage: Option<kurbo::Rect>,
    /// Containment boundaries whose old bounds were captured before a pending
    /// style resolve. Their new bounds complete the damage union afterwards.
    contained_damage: Vec<ContainedDamage>,
    full_repaint: bool,
    /// Style/tree mutations need Stylo and Taffy.
    needs_resolve: bool,
    /// Geometry snapshots are only needed when layout may have changed.
    /// Paint-only declarations such as transforms still resolve and repaint,
    /// but their synchronous DOM geometry is derived from the mirrored style.
    needs_geometry_sync: bool,
    animation_epoch: std::time::Instant,
    /// Solid-created nodes use monotonically increasing JavaScript IDs while
    /// Blitz allocates its own arena IDs. Initial parsed nodes are identity
    /// mapped; these maps keep later mutations and hit testing in agreement.
    script_to_dom: HashMap<usize, usize>,
    dom_to_script: HashMap<usize, usize>,
    /// Authoritative paint-order hover path. Blitz's built-in hit tester does
    /// not account for every hoisted overlay that its painter supports, so
    /// shell pointer routing and CSS hover must share this path.
    pointer_hover_path: Vec<usize>,
}

struct DenoEventHandler<'a> {
    runtime: &'a mut ShellRuntime,
    dom_to_script: &'a HashMap<usize, usize>,
    claimed: Rc<Cell<bool>>,
    ordinary_dom_hit: bool,
    target_override: Option<usize>,
    default_prevented: Option<Rc<Cell<bool>>>,
    suppress_blitz_default: bool,
}

impl EventHandler for DenoEventHandler<'_> {
    fn handle_event(
        &mut self,
        _chain: &[usize],
        event: &mut DomEvent,
        _document: &mut dyn blitz_dom::Document,
        event_state: &mut EventState,
    ) {
        let target = self.target_override.unwrap_or_else(|| {
            self.dom_to_script
                .get(&event.target)
                .copied()
                .unwrap_or(event.target)
        });
        let (event_class, init) = deno_event_init(&event.data);
        let result = self.runtime.dispatch_dom(
            event.name(),
            target,
            event_class,
            &init,
            event.bubbles,
            event.cancelable,
        );
        if result.default_prevented {
            if let Some(default_prevented) = &self.default_prevented {
                default_prevented.set(true);
            }
            event_state.prevent_default();
        }
        if self.suppress_blitz_default {
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

fn modifier_init(modifiers: keyboard_types::Modifiers) -> serde_json::Value {
    serde_json::json!({
        "altKey": modifiers.contains(keyboard_types::Modifiers::ALT),
        "ctrlKey": modifiers.contains(keyboard_types::Modifiers::CONTROL),
        "shiftKey": modifiers.contains(keyboard_types::Modifiers::SHIFT),
        "metaKey": modifiers.contains(keyboard_types::Modifiers::META),
    })
}

fn pointer_init(event: &BlitzPointerEvent) -> serde_json::Value {
    let mut init = modifier_init(event.mods);
    if let Some(init) = init.as_object_mut() {
        init.insert("clientX".into(), event.client_x().into());
        init.insert("clientY".into(), event.client_y().into());
        init.insert("screenX".into(), event.screen_x().into());
        init.insert("screenY".into(), event.screen_y().into());
        init.insert("button".into(), (event.button as u8).into());
        init.insert("buttons".into(), event.buttons.bits().into());
        init.insert("isPrimary".into(), event.is_primary.into());
        init.insert("pressure".into(), event.details.pressure.into());
        let (pointer_id, pointer_type) = match event.id {
            BlitzPointerId::Mouse => (1, "mouse"),
            BlitzPointerId::Pen => (2, "pen"),
            BlitzPointerId::Finger(id) => (id.saturating_add(2), "touch"),
        };
        init.insert("pointerId".into(), pointer_id.into());
        init.insert("pointerType".into(), pointer_type.into());
    }
    init
}

fn deno_event_init(event: &DomEventData) -> (&'static str, serde_json::Value) {
    match event {
        DomEventData::PointerMove(value)
        | DomEventData::PointerDown(value)
        | DomEventData::PointerUp(value)
        | DomEventData::PointerCancel(value)
        | DomEventData::PointerEnter(value)
        | DomEventData::PointerLeave(value)
        | DomEventData::PointerOver(value)
        | DomEventData::PointerOut(value) => ("pointer", pointer_init(value)),
        DomEventData::MouseMove(value)
        | DomEventData::MouseDown(value)
        | DomEventData::MouseUp(value)
        | DomEventData::MouseEnter(value)
        | DomEventData::MouseLeave(value)
        | DomEventData::MouseOver(value)
        | DomEventData::MouseOut(value)
        | DomEventData::Click(value)
        | DomEventData::ContextMenu(value)
        | DomEventData::DoubleClick(value) => ("mouse", pointer_init(value)),
        DomEventData::Wheel(value) => {
            let (delta_x, delta_y, delta_mode) = match value.delta {
                blitz_traits::events::BlitzWheelDelta::Lines(x, y) => (x, y, 1),
                blitz_traits::events::BlitzWheelDelta::Pixels(x, y) => (x, y, 0),
            };
            let mut init = modifier_init(value.mods);
            if let Some(init) = init.as_object_mut() {
                init.insert("clientX".into(), value.client_x().into());
                init.insert("clientY".into(), value.client_y().into());
                init.insert("deltaX".into(), delta_x.into());
                init.insert("deltaY".into(), delta_y.into());
                init.insert("deltaMode".into(), delta_mode.into());
                init.insert("buttons".into(), value.buttons.bits().into());
            }
            ("wheel", init)
        }
        DomEventData::KeyPress(value)
        | DomEventData::KeyDown(value)
        | DomEventData::KeyUp(value) => {
            let mut init = modifier_init(value.modifiers);
            if let Some(init) = init.as_object_mut() {
                init.insert("key".into(), value.key.to_string().into());
                init.insert("code".into(), format!("{:?}", value.code).into());
                init.insert("repeat".into(), value.is_auto_repeating.into());
                init.insert("isComposing".into(), value.is_composing.into());
            }
            ("keyboard", init)
        }
        DomEventData::Input(value) => (
            "input",
            serde_json::json!({ "data": value.value, "inputType": "insertText" }),
        ),
        DomEventData::Ime(value) => {
            let data = match value {
                blitz_traits::events::BlitzImeEvent::Preedit(value, _)
                | blitz_traits::events::BlitzImeEvent::Commit(value) => value.as_str(),
                _ => "",
            };
            ("composition", serde_json::json!({ "data": data }))
        }
        DomEventData::Focus(_)
        | DomEventData::Blur(_)
        | DomEventData::FocusIn(_)
        | DomEventData::FocusOut(_) => ("focus", serde_json::json!({})),
        DomEventData::Scroll(value) => (
            "event",
            serde_json::json!({
                "scrollTop": value.scroll_top,
                "scrollLeft": value.scroll_left,
            }),
        ),
        DomEventData::TouchStart(value)
        | DomEventData::TouchMove(value)
        | DomEventData::TouchEnd(value)
        | DomEventData::TouchCancel(value) => ("pointer", pointer_init(value)),
        DomEventData::AppleStandardKeybinding(value) => {
            ("event", serde_json::json!({ "binding": value.as_str() }))
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

/// Factory for a shell document and its native network/window providers.
#[derive(Clone)]
pub struct ShellDocumentFactory {
    plugin_runtime: Arc<snowglass_pluginhost::PluginRuntime>,
    event_proxy: crate::window_manager::EventLoopProxy<crate::app::UserEvent>,
}

impl ShellDocumentFactory {
    pub fn new(
        plugin_registry: snowglass_pluginhost::PluginRegistry,
        event_proxy: crate::window_manager::EventLoopProxy<crate::app::UserEvent>,
    ) -> Self {
        Self {
            plugin_runtime: Arc::new(snowglass_pluginhost::PluginRuntime::new(
                plugin_registry,
                snowglass_pluginhost::PluginRuntimeOptions::default(),
            )),
            event_proxy,
        }
    }

    pub fn plugin_runtime(&self) -> Arc<snowglass_pluginhost::PluginRuntime> {
        self.plugin_runtime.clone()
    }

    /// `views` is how many game views the document can show; it fixes the
    /// number of chrome bands, which is one more than that. The document names
    /// each one by its `<tracked-surface src>`, so this is a capacity, not a
    /// mapping.
    pub async fn create(
        self,
        html: &str,
        base_url: String,
        views: usize,
        width: u32,
        height: u32,
        emulator_commands: EmulatorCommandSink,
        launcher_commands: LauncherCommandSink,
        initial_launcher_state: serde_json::Value,
    ) -> Result<(ShellDocument, ShellRuntime), Box<dyn std::error::Error>> {
        let (sender, arrivals) = channel();
        let proxy = self.event_proxy.clone();
        let waker: Arc<dyn NetWaker> = Arc::new(move |_| {
            let _ = sender.send(());
            let _ = proxy.send_event(crate::commands::UserEvent::Window(
                crate::window_manager::WindowCommand::RedrawShell,
            ));
        });
        let net: Arc<dyn NetProvider> = Arc::new(ShellNetProvider {
            upstream: blitz_net::Provider::shared(Some(waker.clone())),
            waker,
        });
        let shell_provider: Arc<dyn ShellProvider> =
            Arc::new(NativeShellProvider::new(self.event_proxy.clone()));
        let mut document = blitz_html::HtmlDocument::from_html(
            html,
            blitz_dom::DocumentConfig {
                base_url: Some(base_url.clone()),
                net_provider: Some(net),
                shell_provider: Some(shell_provider),
                ..Default::default()
            },
        );
        initialise_tracked_surfaces(&mut document);
        self.build(
            document,
            arrivals,
            html,
            &base_url,
            views,
            (width, height),
            emulator_commands,
            launcher_commands,
            initial_launcher_state,
        )
        .await
    }
}

impl ShellDocumentFactory {
    async fn build(
        self,
        document: blitz_html::HtmlDocument,
        arrivals: Receiver<()>,
        html: &str,
        source_name: &str,
        views: usize,
        viewport: (u32, u32),
        emulator_commands: EmulatorCommandSink,
        launcher_commands: LauncherCommandSink,
        initial_launcher_state: serde_json::Value,
    ) -> Result<(ShellDocument, ShellRuntime), Box<dyn std::error::Error>> {
        let (width, height) = viewport;
        let script_to_dom = document
            .as_ref()
            .tree()
            .iter()
            .map(|(id, _)| (id, id))
            .collect::<HashMap<_, _>>();
        let dom_to_script = script_to_dom.clone();
        let mut layer = ShellDocument {
            document,
            arrivals,
            bands: (0..=views).map(|_| Scene::new()).collect(),
            placements: Vec::new(),
            width: 0,
            height: 0,
            dirty: true,
            partial_damage: None,
            contained_damage: Vec::new(),
            full_repaint: true,
            needs_resolve: true,
            needs_geometry_sync: true,
            animation_epoch: std::time::Instant::now(),
            script_to_dom,
            dom_to_script,
            pointer_hover_path: Vec::new(),
        };
        layer.resize(width, height);
        // Scripts run after parsing, with a complete DOM just like deferred
        // document scripts. Resolve once first so geometry APIs have useful
        // values during initialisation.
        layer.document.as_mut().resolve(0.0);
        let mut runtime = ShellRuntime::start(
            layer.document.as_ref(),
            layer.width,
            layer.height,
            html.to_owned(),
            source_name.to_owned(),
            emulator_commands,
            launcher_commands,
            initial_launcher_state,
        )
        .await?;
        layer.apply_script_mutations(&mut runtime);
        Ok((layer, runtime))
    }
}

impl ShellDocument {
    pub fn resize(&mut self, width: u32, height: u32) {
        let (width, height) = (width.max(1), height.max(1));
        if (self.width, self.height) == (width, height) {
            return;
        }
        self.width = width;
        self.height = height;
        // Absolutely positioned boxes resolve against the viewport, so without
        // one every box in this layer lays out at zero size.
        self.document
            .as_mut()
            .set_viewport(blitz_traits::shell::Viewport::new(
                width,
                height,
                1.0,
                blitz_traits::shell::ColorScheme::Dark,
            ));
        self.dirty = true;
        self.full_repaint = true;
        self.partial_damage = None;
        self.contained_damage.clear();
        self.needs_resolve = true;
        self.needs_geometry_sync = true;
    }

    pub fn placements(&self) -> &[GamePlacement] {
        &self.placements
    }

    /// Forces the next paint even when the document itself did not change.
    ///
    /// This is also needed after the merged compositor moves to another GPU,
    /// because the replacement device must create fresh local chrome textures.
    pub fn invalidate(&mut self) {
        self.dirty = true;
        self.full_repaint = true;
        self.partial_damage = None;
        self.contained_damage.clear();
    }

    pub fn needs_paint(&self) -> bool {
        self.dirty
    }

    fn animation_time(&self) -> f64 {
        self.animation_epoch.elapsed().as_secs_f64()
    }

    pub fn hit_test(&self, x: f32, y: f32) -> Option<ShellHitSnapshot> {
        let painted = pointer_hit(self.document.as_ref(), x, y)?;
        // Blitz may return generated paint/layout elements which have no
        // identity-preserving JavaScript wrapper. Dispatching one of those
        // nodes makes the Deno bridge fall back to `document`, so delegated
        // Solid/Ark handlers never see the button underneath. Browser event
        // targets are DOM nodes: promote the paint hit to the nearest mapped
        // DOM ancestor before handing it to the event driver.
        let target = std::iter::successors(Some(painted), |node| {
            self.document
                .as_ref()
                .get_node(*node)
                .and_then(|node| node.parent)
        })
        .find(|node| self.dom_to_script.contains_key(node))?;
        let over_tracked_surface = node_is_within_tracked_surface(self.document.as_ref(), target);
        Some(ShellHitSnapshot {
            node_id: target,
            over_tracked_surface,
        })
    }

    fn set_pointer_hover(&mut self, target: Option<usize>) {
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
                    .snapshot_node_and(node, |node| node.unhover());
            }
        }
        for node in next_path
            .iter()
            .copied()
            .filter(|node| !previous_path.contains(node))
        {
            self.document
                .as_mut()
                .snapshot_node_and(node, |node| node.hover());
        }
        self.pointer_hover_path = next_path;
        self.dirty = true;
        self.full_repaint = true;
        self.partial_damage = None;
        self.contained_damage.clear();
        self.needs_resolve = true;
        // Hover selectors may legally change layout, so keep this conservative.
        self.needs_geometry_sync = true;
    }

    pub fn clear_pointer_hover(&mut self) {
        self.set_pointer_hover(None);
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
        let click_data = (kind == "mouseup").then(|| match &event {
            Some(UiEvent::PointerUp(data)) => data.clone(),
            _ => unreachable!(),
        });
        let claimed = Rc::new(Cell::new(false));
        if let Some(event) = event {
            // A captured drag keeps delivering pointer events to its original
            // target. Recomputing :hover against the lagging painted position
            // during that sequence both disagrees with capture retargeting and
            // forces a full Stylo/Taffy resolve on otherwise paint-only moves.
            if kind != "mousemove" || buttons == 0 {
                self.set_pointer_hover(Some(hit.node_id));
            }
            let handler = DenoEventHandler {
                runtime,
                dom_to_script: &self.dom_to_script,
                claimed: claimed.clone(),
                ordinary_dom_hit: !hit.over_tracked_surface,
                target_override: None,
                default_prevented: None,
                suppress_blitz_default: false,
            };
            let (pointer, mouse) = match event {
                UiEvent::PointerDown(data) => (
                    DomEventData::PointerDown(data.clone()),
                    DomEventData::MouseDown(data),
                ),
                UiEvent::PointerUp(data) => (
                    DomEventData::PointerUp(data.clone()),
                    DomEventData::MouseUp(data),
                ),
                UiEvent::PointerMove(data) => (
                    DomEventData::PointerMove(data.clone()),
                    DomEventData::MouseMove(data),
                ),
                _ => unreachable!(),
            };
            let mut driver = EventDriver::new(&mut self.document, handler);
            driver.handle_dom_event(DomEvent::new(hit.node_id, pointer));
            driver.handle_dom_event(DomEvent::new(hit.node_id, mouse));
        }
        // Blitz 0.3 can paint a dynamically inserted overlay while its own
        // hit tester still resolves the pointer to the document root. Use the
        // paint-order hit computed above only when Blitz/Deno did not handle
        // the mouse-up, so ordinary controls are never invoked twice and a
        // tracked surface never gains a synthetic UI click.
        if let Some(data) = click_data
            && !hit.over_tracked_surface
            && !claimed.get()
        {
            let target = self
                .dom_to_script
                .get(&hit.node_id)
                .copied()
                .unwrap_or(hit.node_id);
            let result =
                runtime.dispatch_dom("click", target, "mouse", &pointer_init(&data), true, true);
            if result.default_prevented || result.handled {
                claimed.set(true);
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
            // Blitz 0.3's wheel default action ignores DomEvent::target and
            // scrolls from its private hover cache. The shell hit is more
            // accurate for hoisted overlays, so run the default below.
            suppress_blitz_default: true,
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
        let mut generated = Vec::new();
        if !default_prevented.get() {
            self.document.as_mut().scroll_by(
                Some(hit.node_id),
                event.horizontal,
                event.vertical,
                &mut |event| generated.push(event),
            );
        }
        if !generated.is_empty() {
            let handler = DenoEventHandler {
                runtime,
                dom_to_script: &self.dom_to_script,
                claimed: claimed.clone(),
                ordinary_dom_hit: !hit.over_tracked_surface,
                target_override: None,
                default_prevented: None,
                suppress_blitz_default: false,
            };
            let mut driver = EventDriver::new(&mut self.document, handler);
            for event in generated {
                driver.handle_dom_event(event);
            }
        }
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
            suppress_blitz_default: false,
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
            suppress_blitz_default: false,
        };
        EventDriver::new(&mut self.document, handler).handle_ui_event(if event.kind == "keydown" {
            UiEvent::KeyDown(data)
        } else {
            UiEvent::KeyUp(data)
        });
        self.apply_script_mutations(runtime);
        claimed.get()
    }

    /// Verifies that the attachment is the shell tracked-surface element
    /// whose `src` names this tracked render-target index.
    pub fn validates_attachment(&self, attachment: usize, index: u32) -> bool {
        let dom_node = self.script_to_dom.get(&attachment).copied();
        let element = dom_node
            .and_then(|node| self.document.as_ref().get_node(node))
            .and_then(blitz_dom::Node::element_data);
        let valid = element.is_some_and(|element| {
            is_tracked_surface_element(element)
                && element
                    .attr(local_name!("src"))
                    .and_then(|source| source.parse::<u32>().ok())
                    == Some(index)
        });
        if !valid {
            tracing::warn!(
                attachment,
                index,
                ?dom_node,
                tag = element.map(|element| element.name.local.as_ref()),
                source = element.and_then(|element| element.attr(local_name!("src"))),
                "snowglass: invalid shell surface attachment"
            );
        }
        valid
    }

    /// Re-lays out and publishes paint scenes when the shell changed.
    pub fn produce_frame(&mut self, runtime: &mut ShellRuntime) -> ShellFrame {
        let assets_arrived = self.take_arrivals();
        if !assets_arrived && !self.dirty {
            return ShellFrame {
                chrome_bands: Vec::new(),
                width: self.width,
                height: self.height,
                dirty: false,
                damage: None,
                dirty_bands: None,
            };
        }
        self.dirty = false;
        // Resolving is what issues the fetches, and this provider serves them
        // before returning, so anything the document asked for is waiting by
        // the time it returns. Resolving again until nothing new arrives means
        // an asset that only became reachable through another one still makes
        // this paint, rather than waiting for an unrelated event to trigger the
        // next. The bound is there because a document that somehow kept asking
        // for more must not spin the shell.
        if assets_arrived {
            self.full_repaint = true;
            self.partial_damage = None;
            self.contained_damage.clear();
        }
        if assets_arrived || self.needs_resolve {
            self.needs_resolve = false;
            let resolve_started = std::time::Instant::now();
            let animation_time = self.animation_time();
            self.document.as_mut().resolve(animation_time);
            for _ in 0..ASSET_ROUNDS {
                if !self.take_arrivals() {
                    break;
                }
                let animation_time = self.animation_time();
                self.document.as_mut().resolve(animation_time);
            }
            let resolved = resolve_started.elapsed();
            let sync_started = std::time::Instant::now();
            if std::mem::take(&mut self.needs_geometry_sync) {
                let geometry_roots = (!self.full_repaint)
                    .then(|| {
                        self.contained_damage
                            .iter()
                            .map(|damage| damage.node)
                            .collect::<Vec<_>>()
                    })
                    .filter(|roots| !roots.is_empty());
                runtime.sync(
                    self.document.as_ref(),
                    self.width,
                    self.height,
                    &self.dom_to_script,
                    geometry_roots.as_deref(),
                );
            }
            if std::env::var_os(PAINT_TIMING).is_some() {
                tracing::error!(
                    "snowglass: shell resolve {:.2}ms sync {:.2}ms",
                    resolved.as_secs_f64() * 1000.0,
                    sync_started.elapsed().as_secs_f64() * 1000.0,
                );
            }
            if !self.full_repaint {
                for contained in self.contained_damage.drain(..) {
                    let after = visual_damage_rect(self.document.as_ref(), contained.node);
                    let damage = contained.before.union(after);
                    self.partial_damage = Some(
                        self.partial_damage
                            .map_or(damage, |current| current.union(damage)),
                    );
                }
            } else {
                self.contained_damage.clear();
            }
        }
        // Blitz counts every custom widget as permanently animating. A tracked
        // surface is a retained compositor placeholder, not animated shell
        // chrome, so `is_animating()` would re-run Stylo and repaint the whole
        // document forever. Script mutations and native UI events explicitly
        // invalidate the shell when its chrome actually changes.

        let started = std::time::Instant::now();
        let (used, overflowed) = {
            // The painters are a view onto the scenes, so they are built for
            // this pass and dropped before the scenes are rasterised.
            let mut painters = self
                .bands
                .iter_mut()
                .map(VelloScenePainter::new)
                .collect::<Vec<_>>();
            let mut scene = BandedScene::new(&mut painters, &mut self.placements);
            // blitz-paint 0.3 deliberately leaves scene lifetime to its
            // caller. Reusing our retained Vello scenes without resetting
            // them accumulates every prior drawer position, border and shadow
            // across resize/drag animation frames.
            scene.reset();
            blitz_paint::paint_scene(
                &mut scene,
                self.document.as_mut(),
                1.0,
                self.width,
                self.height,
                0,
                0,
            );
            (scene.bands_used(), scene.overflowed())
        };
        apply_tracked_surface_styles(self.document.as_ref(), &mut self.placements);
        if overflowed {
            tracing::error!(
                "snowglass: the game layer shows more than {} views, so the ones past that \
                 cannot be stacked against the chrome between them",
                self.bands.len() - 1
            );
        }
        let encoded = started.elapsed();
        if std::env::var_os(PAINT_TIMING).is_some() {
            tracing::error!(
                "snowglass: game layer encoded {}x{} in {:.2}ms over {used} band(s)",
                self.width,
                self.height,
                encoded.as_secs_f64() * 1000.0,
            );
        }
        let damage = (!self.full_repaint)
            .then_some(self.partial_damage)
            .flatten();
        self.full_repaint = false;
        self.partial_damage = None;
        ShellFrame {
            chrome_bands: self.bands[..used].to_vec(),
            width: self.width,
            height: self.height,
            dirty: true,
            damage,
            // A damaged effect can cross any tracked-surface split. Repaint
            // the clipped rectangle in every band so shadows and other visual
            // overflow cannot leave stale pixels in a supposedly unrelated
            // layer.
            dirty_bands: None,
        }
    }
}

impl ShellDocument {
    /// Hands the document every asset that has finished loading, reporting
    /// whether any did.
    fn take_arrivals(&mut self) -> bool {
        let mut arrived = false;
        while self.arrivals.try_recv().is_ok() {
            arrived = true;
        }
        arrived
    }

    pub(crate) fn apply_script_mutations(&mut self, runtime: &mut ShellRuntime) {
        let mutations = runtime.take_mutations();
        if mutations.is_empty() {
            return;
        }
        let effect = apply_dom_mutations(
            &mut self.document,
            &mut self.script_to_dom,
            &mut self.dom_to_script,
            mutations,
        );
        if !effect.needs_resolve {
            return;
        }
        self.dirty = true;
        self.needs_resolve |= effect.needs_resolve;
        self.needs_geometry_sync |= effect.needs_geometry_sync;
        if effect.unbounded {
            self.full_repaint = true;
            self.partial_damage = None;
            self.contained_damage.clear();
        } else if !self.full_repaint {
            self.contained_damage.extend(effect.contained);
        }
    }
}

#[derive(Default)]
struct MutationEffect {
    needs_resolve: bool,
    needs_geometry_sync: bool,
    unbounded: bool,
    contained: Vec<ContainedDamage>,
}

#[derive(Clone, Copy)]
struct ContainedDamage {
    node: usize,
    before: kurbo::Rect,
}

fn apply_dom_mutations(
    document: &mut blitz_html::HtmlDocument,
    script_to_dom: &mut HashMap<usize, usize>,
    dom_to_script: &mut HashMap<usize, usize>,
    mutations: Vec<Mutation>,
) -> MutationEffect {
    if mutations.is_empty() {
        return MutationEffect::default();
    }

    // Stylo assumes every descendant reached by attribute invalidation already
    // has a primary computed style. Solid can append a newly-created subtree
    // and then set an attribute on one of its ancestors in the same mutation
    // batch. If we feed that order directly to Blitz, the later invalidation
    // sees the attached-but-unstyled descendants and panics in
    // `ElementStyles::primary`.
    //
    // Attach the new tree without attribute invalidations, give it its base
    // styles, then apply attributes. The next normal paint resolve computes
    // the final styles; the intermediate resolve is never presented.
    let (focus, mutations): (Vec<_>, Vec<_>) = mutations
        .into_iter()
        .partition(|mutation| matches!(mutation, Mutation::Focus { .. }));
    let has_new_elements = mutations
        .iter()
        .any(|mutation| matches!(mutation, Mutation::CreateElement { .. }));
    let mut effect = if has_new_elements {
        let (attributes, structure): (Vec<_>, Vec<_>) =
            mutations.into_iter().partition(|mutation| {
                matches!(
                    mutation,
                    Mutation::SetAttribute { .. } | Mutation::RemoveAttribute { .. }
                )
            });
        apply_dom_mutation_batch(document, script_to_dom, dom_to_script, structure);
        document.as_mut().resolve(1.0);
        let mut effect =
            apply_dom_mutation_batch(document, script_to_dom, dom_to_script, attributes);
        effect.needs_resolve = true;
        effect.needs_geometry_sync = true;
        effect.unbounded = true;
        effect.contained.clear();
        effect
    } else {
        apply_dom_mutation_batch(document, script_to_dom, dom_to_script, mutations)
    };
    if !focus.is_empty() {
        effect.needs_resolve = true;
        effect.needs_geometry_sync = true;
        effect.unbounded = true;
        effect.contained.clear();
    }
    for mutation in focus {
        if let Mutation::Focus { node } = mutation
            && let Some(node) = script_to_dom.get(&node).copied()
        {
            document.as_mut().set_focus_to(node);
        }
    }
    effect
}

fn apply_dom_mutation_batch(
    document: &mut blitz_html::HtmlDocument,
    script_to_dom: &mut HashMap<usize, usize>,
    dom_to_script: &mut HashMap<usize, usize>,
    mutations: Vec<Mutation>,
) -> MutationEffect {
    let mut needs_resolve = false;
    let mut needs_geometry_sync = false;
    let mut unbounded_damage = false;
    let mut contained_damage = HashMap::<usize, kurbo::Rect>::new();
    let mut mutator = document.as_mut().mutate();
    for mutation in mutations {
        match mutation {
            Mutation::CreateElement {
                node,
                tag,
                namespace,
            } => {
                needs_resolve = true;
                needs_geometry_sync = true;
                unbounded_damage = true;
                let is_tracked_surface = tag == "tracked-surface";
                let namespace = match namespace.as_deref() {
                    Some("http://www.w3.org/2000/svg") => ns!(svg),
                    _ => ns!(html),
                };
                let actual = mutator.create_element(
                    QualName::new(None, namespace, LocalName::from(tag)),
                    Vec::new(),
                );
                if is_tracked_surface {
                    mutator
                        .doc
                        .set_custom_widget(actual, Box::new(TrackedSurfaceWidget::default()));
                }
                script_to_dom.insert(node, actual);
                dom_to_script.insert(actual, node);
            }
            Mutation::CreateText { node, text } => {
                needs_resolve = true;
                needs_geometry_sync = true;
                unbounded_damage = true;
                let actual = mutator.create_text_node(&text);
                script_to_dom.insert(node, actual);
                dom_to_script.insert(actual, node);
            }
            Mutation::CreateComment { node } => {
                needs_resolve = true;
                needs_geometry_sync = true;
                unbounded_damage = true;
                let actual = mutator.create_comment_node();
                script_to_dom.insert(node, actual);
                dom_to_script.insert(actual, node);
            }
            Mutation::AppendChild { parent, child } => {
                needs_resolve = true;
                needs_geometry_sync = true;
                unbounded_damage = true;
                let Some((parent, child)) = script_to_dom
                    .get(&parent)
                    .copied()
                    .zip(script_to_dom.get(&child).copied())
                else {
                    continue;
                };
                mutator.append_children(parent, &[child]);
            }
            Mutation::InsertBefore {
                parent,
                child,
                before,
            } => {
                needs_resolve = true;
                needs_geometry_sync = true;
                unbounded_damage = true;
                let Some((parent, child, before)) = script_to_dom
                    .get(&parent)
                    .copied()
                    .zip(script_to_dom.get(&child).copied())
                    .zip(script_to_dom.get(&before).copied())
                    .map(|((parent, child), before)| (parent, child, before))
                else {
                    continue;
                };
                if mutator.parent_id(before) == Some(parent) {
                    mutator.insert_nodes_before(before, &[child]);
                }
            }
            Mutation::RemoveChild { parent, child } => {
                needs_resolve = true;
                needs_geometry_sync = true;
                unbounded_damage = true;
                let Some((parent, child)) = script_to_dom
                    .get(&parent)
                    .copied()
                    .zip(script_to_dom.get(&child).copied())
                else {
                    continue;
                };
                if mutator.parent_id(child) == Some(parent) {
                    mutator.remove_node(child);
                }
            }
            Mutation::SetText { node, text } => {
                needs_resolve = true;
                needs_geometry_sync = true;
                unbounded_damage = true;
                if let Some(node) = script_to_dom.get(&node).copied() {
                    mutator.set_node_text(node, &text);
                }
            }
            Mutation::SetAttribute { node, name, value } => {
                let Some(node) = script_to_dom.get(&node).copied() else {
                    continue;
                };
                let attribute_name = LocalName::from(name);
                let previous = mutator
                    .doc
                    .get_node(node)
                    .and_then(blitz_dom::Node::element_data)
                    .and_then(|element| element.attr(attribute_name.clone()))
                    .map(str::to_owned);
                if previous.as_deref() == Some(value.as_str()) {
                    continue;
                }
                needs_resolve = true;
                needs_geometry_sync |= attribute_name.as_ref() != "style"
                    || style_change_requires_geometry(previous.as_deref(), Some(&value));
                if let Some(contained) = containment_damage(mutator.doc, node) {
                    contained_damage
                        .entry(contained.node)
                        .or_insert(contained.before);
                } else {
                    unbounded_damage = true;
                }
                // HTML attributes are in the null namespace. Using the HTML
                // element namespace here creates a second attribute instead
                // of replacing one parsed from markup, leaving Stylo and hit
                // testing on the stale value while the JavaScript mirror sees
                // the update.
                mutator.set_attribute(node, QualName::new(None, ns!(), attribute_name), &value);
            }
            Mutation::RemoveAttribute { node, name } => {
                let Some(node) = script_to_dom.get(&node).copied() else {
                    continue;
                };
                let attribute_name = LocalName::from(name);
                let qualified_name = QualName::new(None, ns!(), attribute_name.clone());
                if mutator
                    .doc
                    .get_node(node)
                    .and_then(blitz_dom::Node::element_data)
                    .and_then(|element| element.attr(attribute_name.clone()))
                    .is_none()
                {
                    continue;
                }
                needs_resolve = true;
                needs_geometry_sync |= attribute_name.as_ref() != "style"
                    || style_change_requires_geometry(
                        mutator
                            .doc
                            .get_node(node)
                            .and_then(blitz_dom::Node::element_data)
                            .and_then(|element| element.attr(attribute_name.clone())),
                        None,
                    );
                if let Some(contained) = containment_damage(mutator.doc, node) {
                    contained_damage
                        .entry(contained.node)
                        .or_insert(contained.before);
                } else {
                    unbounded_damage = true;
                }
                mutator.clear_attribute(node, qualified_name);
            }
            Mutation::Focus { .. } => unreachable!("focus mutations are applied after DOM edits"),
        }
    }
    MutationEffect {
        needs_resolve,
        needs_geometry_sync,
        unbounded: unbounded_damage,
        contained: contained_damage
            .into_iter()
            .map(|(node, before)| ContainedDamage { node, before })
            .collect(),
    }
}

/// Returns whether an inline-style edit can alter layout geometry. CSS
/// transforms and opacity affect paint/compositing only; all other changed
/// declarations remain conservative because custom properties and uncommon
/// properties may feed layout.
fn style_change_requires_geometry(previous: Option<&str>, current: Option<&str>) -> bool {
    fn declarations(style: Option<&str>) -> HashMap<String, String> {
        style
            .unwrap_or_default()
            .split(';')
            .filter_map(|declaration| declaration.split_once(':'))
            .map(|(name, value)| (name.trim().to_ascii_lowercase(), value.trim().to_owned()))
            .collect()
    }

    let previous = declarations(previous);
    let current = declarations(current);
    previous
        .keys()
        .chain(current.keys())
        .filter(|name| !matches!(name.as_str(), "transform" | "transform-origin" | "opacity"))
        .any(|name| previous.get(name) != current.get(name))
}

/// Finds the nearest standards-defined paint boundary for a mutation. Layout
/// and paint containment together guarantee that descendant layout and paint
/// cannot escape this box, so the compositor may preserve pixels outside it.
fn containment_damage(
    document: &blitz_dom::BaseDocument,
    node_id: usize,
) -> Option<ContainedDamage> {
    std::iter::successors(Some(node_id), |node| {
        document.get_node(*node).and_then(|node| node.parent)
    })
    .find_map(|node| {
        let contain = document
            .get_node(node)?
            .primary_styles()?
            .get_box()
            .clone_contain();
        (contain.contains(Contain::LAYOUT | Contain::PAINT)).then(|| ContainedDamage {
            node,
            before: visual_damage_rect(document, node),
        })
    })
}

/// Returns conservative viewport-space paint bounds. Blitz exposes layout
/// bounds separately from transforms and does not yet publish visual-overflow
/// rectangles, so include translations explicitly and reserve enough fringe
/// for ordinary shadows and antialiasing.
fn visual_damage_rect(document: &blitz_dom::BaseDocument, node_id: usize) -> kurbo::Rect {
    const VISUAL_OVERFLOW_MARGIN: f64 = 64.0;
    let Some(rect) = document.get_client_bounding_rect(node_id) else {
        return kurbo::Rect::ZERO;
    };
    let mut translation = kurbo::Vec2::ZERO;
    for node in std::iter::successors(Some(node_id), |node| {
        document.get_node(*node).and_then(|node| node.parent)
    }) {
        if let Some(transform) = document.get_node(node).and_then(|node| node.transform) {
            let coefficients = transform.as_coeffs();
            translation += kurbo::Vec2::new(coefficients[4], coefficients[5]);
        }
    }
    kurbo::Rect::new(
        rect.x + translation.x - VISUAL_OVERFLOW_MARGIN,
        rect.y + translation.y - VISUAL_OVERFLOW_MARGIN,
        rect.x + translation.x + rect.width + VISUAL_OVERFLOW_MARGIN,
        rect.y + translation.y + rect.height + VISUAL_OVERFLOW_MARGIN,
    )
}

fn apply_tracked_surface_styles(
    document: &blitz_dom::BaseDocument,
    placements: &mut [GamePlacement],
) {
    let styles = document
        .tree()
        .iter()
        .filter_map(|(node_id, node)| {
            let element = node.element_data()?;
            if !is_tracked_surface_element(element) {
                return None;
            }
            let source = element.attr(local_name!("src"))?.parse::<u64>().ok()?;
            let radius = element
                .attr(LocalName::from("data-corner-radius"))
                .and_then(|value| value.parse::<f32>().ok())
                .unwrap_or_default()
                .max(0.0);
            let styles = node.primary_styles()?;
            Some((
                source,
                (
                    node_id,
                    radius,
                    GameViewStyle {
                        object_fit: styles.clone_object_fit(),
                        object_position: styles.clone_object_position(),
                    },
                ),
            ))
        })
        .collect::<HashMap<_, _>>();
    for placement in placements {
        let maximum = (placement.rect.width().min(placement.rect.height()) * 0.5) as f32;
        placement.corner_radius = styles
            .get(&placement.source_id)
            .map(|(_, radius, _)| *radius)
            .unwrap_or_default()
            .min(maximum);
        placement.style = styles
            .get(&placement.source_id)
            .map(|(_, _, style)| style.clone());
        placement.node_id = styles
            .get(&placement.source_id)
            .map(|(node_id, _, _)| *node_id);
        placement.opacity = placement.opacity.clamp(0.0, 1.0);
    }
}

fn initialise_tracked_surfaces(document: &mut blitz_html::HtmlDocument) {
    let surfaces = document
        .as_mut()
        .tree()
        .iter()
        .filter_map(|(node, current)| {
            let element = current.element_data()?;
            if element.name.local.as_ref() != "tracked-surface" {
                return None;
            }
            let source = element.attr(local_name!("src"))?.parse::<u64>().ok()?;
            Some((node, source))
        })
        .collect::<Vec<_>>();
    for (node, source) in surfaces {
        set_tracked_surface_source(document, node, Some(source));
    }
}

fn set_tracked_surface_source(
    document: &mut blitz_html::HtmlDocument,
    node: usize,
    source: Option<u64>,
) {
    document
        .as_mut()
        .set_custom_widget(node, Box::new(TrackedSurfaceWidget::new(source)));
}

fn is_tracked_surface_element(element: &blitz_dom::node::ElementData) -> bool {
    element.name.local.as_ref() == "tracked-surface"
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::shell_runtime::deno;

    #[test]
    fn tracked_surface_becomes_a_custom_paint_placeholder() {
        let mut document = blitz_html::HtmlDocument::from_html(
            r#"<style>.surface {
                display: block;
                width: 320px;
                height: 180px;
                object-fit: cover;
                object-position: 25% 75%;
                opacity: 0.4;
            }</style><tracked-surface class="surface" src="7" data-corner-radius="12"></tracked-surface>"#,
            blitz_dom::DocumentConfig::default(),
        );
        initialise_tracked_surfaces(&mut document);
        document
            .as_mut()
            .set_viewport(blitz_traits::shell::Viewport::new(
                800,
                600,
                1.0,
                blitz_traits::shell::ColorScheme::Dark,
            ));
        document.as_mut().resolve(1.0);
        let (surface_id, surface) = document
            .as_ref()
            .tree()
            .iter()
            .find_map(|(node_id, node)| {
                node.element_data()
                    .filter(|element| is_tracked_surface_element(element))
                    .map(|element| (node_id, element))
            })
            .unwrap();
        assert!(surface.custom_widget_data().is_some());
        let mut scenes = (0..2).map(|_| Scene::new()).collect::<Vec<_>>();
        let mut painters = scenes
            .iter_mut()
            .map(VelloScenePainter::new)
            .collect::<Vec<_>>();
        let mut placements = Vec::new();
        let mut scene = BandedScene::new(&mut painters, &mut placements);
        blitz_paint::paint_scene(&mut scene, document.as_mut(), 1.0, 800, 600, 0, 0);
        apply_tracked_surface_styles(document.as_ref(), &mut placements);
        assert_eq!(placements.len(), 1);
        assert_eq!(placements[0].node_id, Some(surface_id));
        assert_eq!(placements[0].source_id, 7);
        assert_eq!(placements[0].rect.width(), 320.0);
        assert_eq!(placements[0].rect.height(), 180.0);
        let clipping = placements[0]
            .clipping
            .expect("the containing paint layer publishes its clip");
        assert!(clipping.contains(placements[0].rect.origin()));
        assert_eq!(placements[0].corner_radius, 12.0);
        assert!((placements[0].opacity - 0.4).abs() < f32::EPSILON);
        let style = placements[0].style.as_ref().unwrap();
        assert_eq!(
            style.object_fit,
            style::properties::generated::longhands::object_fit::computed_value::T::Cover
        );
        assert_eq!(
            style
                .object_position
                .horizontal
                .resolve(style::values::computed::CSSPixelLength::new(100.0))
                .px(),
            25.0
        );
        assert_eq!(
            style
                .object_position
                .vertical
                .resolve(style::values::computed::CSSPixelLength::new(100.0))
                .px(),
            75.0
        );
    }

    #[test]
    fn pointer_hit_skips_disabled_overlays_but_keeps_reenabled_children() {
        let html = r#"
            <style>
              :root, body { width: 100%; height: 100%; margin: 0; }
              .surface, .overlay, .shield { position: absolute; inset: 0; }
              .overlay { z-index: 20; pointer-events: none; }
              .shield { pointer-events: none; }
              .button {
                position: absolute;
                left: 10px;
                top: 10px;
                width: 40px;
                height: 40px;
                pointer-events: auto;
              }
            </style>
            <tracked-surface class="surface" id="surface" src="0"></tracked-surface>
            <div class="overlay">
              <div class="shield"></div>
              <div class="button" id="button"></div>
            </div>
        "#;
        let mut document =
            blitz_html::HtmlDocument::from_html(html, blitz_dom::DocumentConfig::default());
        document
            .as_mut()
            .set_viewport(blitz_traits::shell::Viewport::new(
                800,
                600,
                1.0,
                blitz_traits::shell::ColorScheme::Dark,
            ));
        document.as_mut().resolve(1.0);
        let find = |id| {
            document
                .as_ref()
                .tree()
                .iter()
                .find_map(|(node, current)| {
                    (current.attr(local_name!("id")) == Some(id)).then_some(node)
                })
                .unwrap()
        };
        let surface = find("surface");
        let button = find("button");

        assert_eq!(pointer_hit(document.as_ref(), 20.0, 20.0), Some(button));
        assert_eq!(pointer_hit(document.as_ref(), 100.0, 100.0), Some(surface));
    }

    #[test]
    fn pointer_hit_accounts_for_a_transformed_hoisted_panel() {
        let html = r#"
            <style>
              :root, body { width: 100%; height: 100%; margin: 0; }
              tracked-surface { position: absolute; inset: 0; }
              .panel {
                z-index: 20;
              }
              .header { display: flex; height: 32px; justify-content: flex-end; }
              button { width: 32px; height: 32px; }
            </style>
            <tracked-surface src="0"></tracked-surface>
            <div id="panel" class="panel" style="position: fixed; top: 0; left: 0; width: 520px; height: 340px; transform: translate(300px, 100px)"><div class="header"><button id="close">X</button></div></div>
        "#;
        let mut document =
            blitz_html::HtmlDocument::from_html(html, blitz_dom::DocumentConfig::default());
        document
            .as_mut()
            .set_viewport(blitz_traits::shell::Viewport::new(
                1280,
                832,
                1.0,
                blitz_traits::shell::ColorScheme::Dark,
            ));
        document.as_mut().resolve(1.0);
        let button = document
            .as_ref()
            .tree()
            .iter()
            .find_map(|(node, current)| {
                (current.attr(local_name!("id")) == Some("close")).then_some(node)
            })
            .unwrap();
        let panel = document
            .as_ref()
            .tree()
            .iter()
            .find_map(|(node, current)| {
                (current.attr(local_name!("id")) == Some("panel")).then_some(node)
            })
            .unwrap();

        assert_eq!(pointer_hit(document.as_ref(), 804.0, 116.0), Some(button));

        let mut script_to_dom = document
            .as_ref()
            .tree()
            .iter()
            .map(|(id, _)| (id, id))
            .collect::<HashMap<_, _>>();
        let mut dom_to_script = script_to_dom.clone();
        apply_dom_mutations(
            &mut document,
            &mut script_to_dom,
            &mut dom_to_script,
            vec![deno::Mutation::SetAttribute {
                node: panel,
                name: "style".to_owned(),
                value: "position: fixed; top: 0; left: 0; width: 520px; height: 340px; transform: translate(112px, 383px)".to_owned(),
            }],
        );
        document.as_mut().resolve(2.0);

        assert_eq!(pointer_hit(document.as_ref(), 616.0, 399.0), Some(button));
    }

    #[test]
    fn pointer_hit_does_not_let_scroll_overflow_steal_a_header_button() {
        let html = r#"
            <style>
              :root, body { width: 100%; height: 100%; margin: 0; }
              main { display: flex; width: 100%; height: 100%; flex-direction: column; }
              header { display: flex; height: 76px; flex: none; justify-content: flex-end; }
              button { width: 92px; height: 48px; margin: 14px 22px; }
              .layout { display: flex; min-height: 0; flex: 1; }
              section { flex: 1; }
              aside { width: 352px; overflow: auto; }
              .tall { width: 100%; height: 16000px; }
            </style>
            <main>
              <header><button id="start">Start</button></header>
              <div class="layout"><section></section><aside><div class="tall"></div></aside></div>
            </main>
        "#;
        let mut document =
            blitz_html::HtmlDocument::from_html(html, blitz_dom::DocumentConfig::default());
        document
            .as_mut()
            .set_viewport(blitz_traits::shell::Viewport::new(
                1280,
                816,
                1.0,
                blitz_traits::shell::ColorScheme::Dark,
            ));
        document.as_mut().resolve(1.0);
        let button = document
            .as_ref()
            .tree()
            .iter()
            .find_map(|(node, current)| {
                (current.attr(local_name!("id")) == Some("start")).then_some(node)
            })
            .unwrap();
        let hit = pointer_hit(document.as_ref(), 1212.0, 38.0).unwrap();
        assert!(
            std::iter::successors(Some(hit), |node| document
                .as_ref()
                .get_node(*node)
                .and_then(|node| node.parent))
            .any(|node| node == button),
            "the header button was not in the hit target's event path"
        );
    }

    #[test]
    fn contained_style_change_uses_the_nearest_css_paint_boundary() {
        let html = r#"
            <style>
              #island { position: fixed; left: 100px; top: 80px; width: 520px;
                        height: 340px; contain: layout paint; }
            </style>
            <div id="island"><div id="child"></div></div>
        "#;
        let mut document =
            blitz_html::HtmlDocument::from_html(html, blitz_dom::DocumentConfig::default());
        document
            .as_mut()
            .set_viewport(blitz_traits::shell::Viewport::new(
                800,
                600,
                1.0,
                blitz_traits::shell::ColorScheme::Dark,
            ));
        document.as_mut().resolve(1.0);
        let find = |id| {
            document
                .as_ref()
                .tree()
                .iter()
                .find_map(|(node, current)| {
                    (current.attr(local_name!("id")) == Some(id)).then_some(node)
                })
                .unwrap()
        };
        let island = find("island");
        let child = find("child");
        let mut script_to_dom = document
            .as_ref()
            .tree()
            .iter()
            .map(|(id, _)| (id, id))
            .collect::<HashMap<_, _>>();
        let mut dom_to_script = script_to_dom.clone();

        let effect = apply_dom_mutations(
            &mut document,
            &mut script_to_dom,
            &mut dom_to_script,
            vec![deno::Mutation::SetAttribute {
                node: child,
                name: "style".to_owned(),
                value: "color: white".to_owned(),
            }],
        );
        assert!(effect.needs_resolve);
        assert!(!effect.unbounded);
        assert_eq!(effect.contained.len(), 1);
        assert_eq!(effect.contained[0].node, island);
        assert!(effect.contained[0].before.width() > 520.0);
    }

    #[test]
    fn uncontained_style_change_requires_a_full_repaint() {
        let mut document = blitz_html::HtmlDocument::from_html(
            r#"<div id="ordinary"></div>"#,
            blitz_dom::DocumentConfig::default(),
        );
        document.as_mut().resolve(1.0);
        let node = document
            .as_ref()
            .tree()
            .iter()
            .find_map(|(id, node)| (node.attr(local_name!("id")) == Some("ordinary")).then_some(id))
            .unwrap();
        let mut script_to_dom = document
            .as_ref()
            .tree()
            .iter()
            .map(|(id, _)| (id, id))
            .collect::<HashMap<_, _>>();
        let mut dom_to_script = script_to_dom.clone();
        let effect = apply_dom_mutations(
            &mut document,
            &mut script_to_dom,
            &mut dom_to_script,
            vec![deno::Mutation::SetAttribute {
                node,
                name: "class".to_owned(),
                value: "changed".to_owned(),
            }],
        );
        assert!(effect.needs_resolve);
        assert!(effect.unbounded);
        assert!(effect.contained.is_empty());
    }

    #[test]
    fn paint_only_inline_styles_do_not_require_geometry_sync() {
        assert!(!style_change_requires_geometry(
            Some("transform: translate(10px, 20px); opacity: 0.5"),
            Some("transform: translate(30px, 40px); opacity: 0.8"),
        ));
        assert!(style_change_requires_geometry(
            Some("width: 100px; transform: translate(10px, 20px)"),
            Some("width: 120px; transform: translate(30px, 40px)"),
        ));
        assert!(style_change_requires_geometry(
            Some("--panel-x: 10px; transform: translate(10px, 20px)"),
            Some("--panel-x: 30px; transform: translate(30px, 40px)"),
        ));
    }

    #[test]
    fn pointer_hit_finds_a_centered_dialog_action_over_a_surface() {
        let html = r#"
            <style>
              :root, body { width: 100%; height: 100%; margin: 0; }
              main, tracked-surface, .positioner { position: absolute; inset: 0; }
              .positioner { display: flex; align-items: center; justify-content: center; }
              .content { width: 260px; height: 150px; padding: 24px; }
              button { width: 90px; height: 28px; }
            </style>
            <main>
              <tracked-surface src="0"></tracked-surface>
              <div class="positioner"><div class="content"><button id="close">Close game</button></div></div>
            </main>
        "#;
        let mut document =
            blitz_html::HtmlDocument::from_html(html, blitz_dom::DocumentConfig::default());
        document
            .as_mut()
            .set_viewport(blitz_traits::shell::Viewport::new(
                800,
                600,
                1.0,
                blitz_traits::shell::ColorScheme::Dark,
            ));
        document.as_mut().resolve(1.0);
        let button = document
            .as_ref()
            .tree()
            .iter()
            .find_map(|(node, current)| {
                (current.attr(local_name!("id")) == Some("close")).then_some(node)
            })
            .unwrap();
        let rect = document.as_ref().get_client_bounding_rect(button).unwrap();
        let hit = pointer_hit(
            document.as_ref(),
            (rect.x + rect.width / 2.0) as f32,
            (rect.y + rect.height / 2.0) as f32,
        )
        .unwrap();
        assert!(
            std::iter::successors(Some(hit), |node| document
                .as_ref()
                .get_node(*node)
                .and_then(|node| node.parent))
            .any(|node| node == button),
            "the dialog button was not in the hit target's event path"
        );
    }

    #[tokio::test(flavor = "current_thread")]
    async fn native_pointer_sequence_dispatches_a_delegated_click() {
        tokio::task::LocalSet::new()
            .run_until(async {
                let html = r#"
                    <style>
                      :root, body { width: 100%; height: 100%; margin: 0; }
                      button { position: absolute; left: 10px; top: 10px; width: 120px; height: 50px; }
                    </style>
                    <button id="action">Action</button>
                    <script>
                      document.addEventListener("click", event => {
                        let target = event.target;
                        while (target && target.id !== "action") target = target.parentNode;
                        target?.setAttribute("data-clicked", "true");
                      });
                    </script>
                "#;
                let mut document =
                    blitz_html::HtmlDocument::from_html(html, blitz_dom::DocumentConfig::default());
                document
                    .as_mut()
                    .set_viewport(blitz_traits::shell::Viewport::new(
                        800,
                        600,
                        1.0,
                        blitz_traits::shell::ColorScheme::Dark,
                    ));
                document.as_mut().resolve(0.0);
                let script_to_dom = document
                    .as_ref()
                    .tree()
                    .iter()
                    .map(|(id, _)| (id, id))
                    .collect::<HashMap<_, _>>();
                let dom_to_script = script_to_dom.clone();
                let (_, arrivals) = channel();
                let mut layer = ShellDocument {
                    document,
                    arrivals,
                    bands: vec![Scene::new()],
                    placements: Vec::new(),
                    width: 800,
                    height: 600,
                    dirty: true,
                    partial_damage: None,
                    contained_damage: Vec::new(),
                    full_repaint: true,
                    needs_resolve: true,
                    needs_geometry_sync: true,
                    animation_epoch: std::time::Instant::now(),
                    script_to_dom,
                    dom_to_script,
                    pointer_hover_path: Vec::new(),
                };
                let mut runtime = ShellRuntime::start(
                    layer.document.as_ref(),
                    800,
                    600,
                    html.to_owned(),
                    "snowglass:///native-click-test.html".to_owned(),
                    deno::EmulatorCommandSink::default(),
                    LauncherCommandSink::default(),
                    serde_json::json!({ "kind": "idle" }),
                )
                .await
                .unwrap();
                layer.apply_script_mutations(&mut runtime);
                let hit = layer.hit_test(30.0, 30.0).unwrap();
                let event = |kind, buttons| DomMouseEvent {
                    kind,
                    x: 30.0,
                    y: 30.0,
                    button: 0,
                    buttons,
                    ctrl_key: false,
                    shift_key: false,
                };
                layer.mouse_event(&mut runtime, hit, event("mousedown", 1));
                layer.mouse_event(&mut runtime, hit, event("mouseup", 0));

                let clicked = layer.document.as_ref().tree().iter().any(|(_, node)| {
                    node.element_data().is_some_and(|element| {
                        element.attr(local_name!("id")) == Some("action")
                            && element.attr(LocalName::from("data-clicked")) == Some("true")
                    })
                });
                assert!(clicked, "the generated click did not reach the delegated listener");
            })
            .await;
    }

    #[test]
    fn styles_new_solid_subtrees_before_attribute_invalidation() {
        let html = r#"
            <style>
              .stage { display: flex; width: 400px; height: 200px; }
              .child { flex: 1; }
            </style>
            <div id="root"></div>
        "#;
        let mut document =
            blitz_html::HtmlDocument::from_html(html, blitz_dom::DocumentConfig::default());
        document
            .as_mut()
            .set_viewport(blitz_traits::shell::Viewport::new(
                800,
                600,
                1.0,
                blitz_traits::shell::ColorScheme::Dark,
            ));
        document.as_mut().resolve(1.0);
        let root = document
            .as_ref()
            .tree()
            .iter()
            .find_map(|(id, node)| (node.attr(local_name!("id")) == Some("root")).then_some(id))
            .unwrap();
        let mut script_to_dom = document
            .as_ref()
            .tree()
            .iter()
            .map(|(id, _)| (id, id))
            .collect::<HashMap<_, _>>();
        let mut dom_to_script = script_to_dom.clone();
        let stage = script_to_dom.keys().copied().max().unwrap() + 1;
        let child = stage + 1;

        apply_dom_mutations(
            &mut document,
            &mut script_to_dom,
            &mut dom_to_script,
            vec![
                deno::Mutation::CreateElement {
                    node: stage,
                    tag: "section".to_owned(),
                    namespace: None,
                },
                deno::Mutation::CreateElement {
                    node: child,
                    tag: "div".to_owned(),
                    namespace: None,
                },
                deno::Mutation::AppendChild {
                    parent: stage,
                    child,
                },
                deno::Mutation::AppendChild {
                    parent: root,
                    child: stage,
                },
                deno::Mutation::SetAttribute {
                    node: stage,
                    name: "class".to_owned(),
                    value: "stage".to_owned(),
                },
                deno::Mutation::SetAttribute {
                    node: child,
                    name: "class".to_owned(),
                    value: "child".to_owned(),
                },
            ],
        );
        document.as_mut().resolve(1.0);

        let stage = script_to_dom[&stage];
        assert_eq!(
            document
                .as_ref()
                .tree()
                .get(stage)
                .unwrap()
                .attr(local_name!("class")),
            Some("stage")
        );
    }

    #[tokio::test]
    async fn dynamically_inserted_tracked_surfaces_are_placed() {
        let html = r##"
            <style>
              :root, body, #root { width: 100%; height: 100%; margin: 0; }
              .stage { position: absolute; inset: 0; }
              .primary { position: absolute; inset: 16px; }
              .secondary {
                position: absolute;
                right: 24px;
                bottom: 24px;
                width: 200px;
                height: 100px;
              }
              .surface { position: absolute; inset: 0; width: 100%; height: 100%; }
            </style>
            <div id="root"></div>
            <script>
              const stage = document.createElement("section");
              stage.className = "stage";
              for (const [index, className] of [[0, "primary"], [1, "secondary"]]) {
                const frame = document.createElement("div");
                frame.className = className;
                const surface = document.createElement("tracked-surface");
                surface.className = "surface";
                surface.setAttribute("src", String(index));
                frame.appendChild(surface);
                stage.appendChild(frame);
              }
              document.querySelector("#root").appendChild(stage);
            </script>
        "##;
        let mut document =
            blitz_html::HtmlDocument::from_html(html, blitz_dom::DocumentConfig::default());
        document
            .as_mut()
            .set_viewport(blitz_traits::shell::Viewport::new(
                800,
                600,
                1.0,
                blitz_traits::shell::ColorScheme::Dark,
            ));
        document.as_mut().resolve(1.0);
        let mut script_to_dom = document
            .as_ref()
            .tree()
            .iter()
            .map(|(id, _)| (id, id))
            .collect::<HashMap<_, _>>();
        let mut dom_to_script = script_to_dom.clone();
        let mut runtime = deno::Runtime::new(
            document.as_ref(),
            html,
            "snowglass:///test.html",
            800,
            600,
            deno::EmulatorCommandSink::default(),
        )
        .await
        .unwrap();
        apply_dom_mutations(
            &mut document,
            &mut script_to_dom,
            &mut dom_to_script,
            runtime.take_mutations(),
        );
        document.as_mut().resolve(1.0);

        let mut scenes = (0..3).map(|_| Scene::new()).collect::<Vec<_>>();
        let mut painters = scenes
            .iter_mut()
            .map(VelloScenePainter::new)
            .collect::<Vec<_>>();
        let mut placements = Vec::new();
        let mut scene = BandedScene::new(&mut painters, &mut placements);
        blitz_paint::paint_scene(&mut scene, document.as_mut(), 1.0, 800, 600, 0, 0);
        assert_eq!(
            placements
                .iter()
                .map(|placement| placement.source_id)
                .collect::<Vec<_>>(),
            [0, 1]
        );
        assert_eq!(placements[0].rect.width(), 768.0);
        assert_eq!(placements[0].rect.height(), 568.0);
        assert_eq!(placements[1].rect.width(), 200.0);
        assert_eq!(placements[1].rect.height(), 100.0);
    }
}
