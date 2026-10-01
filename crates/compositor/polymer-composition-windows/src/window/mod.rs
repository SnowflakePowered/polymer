//! Windows window, native composition target, overlay and pacing ownership.
#![allow(unsafe_code)]
mod clock;
mod overlay;
use clock::CompositorClock;
use overlay::EguiOverlay;
use polymer_renderer::Extent;
use polymer_renderer::egui::InputDiagnostics;
use polymer_statistics::RecorderHandle;
use raw_window_handle::{HasWindowHandle, RawWindowHandle};
use std::{error::Error, ffi::c_void, sync::Arc, time::Instant};
use windows::{
    UI::Composition::{
        CompositionStretch, Compositor, ContainerVisual, Core::CompositorController,
        Desktop::DesktopWindowTarget, SpriteVisual,
    },
    Win32::{
        Foundation::HWND,
        System::WinRT::Composition::{ICompositorDesktopInterop, ICompositorInterop},
    },
    core::{IUnknown, Interface},
};
use windows_numerics::Vector2;

use polymer_composition::WindowCommand;
use winit::{
    event::WindowEvent,
    window::{Window, WindowId},
};
/// Sole owner of the native shell window and all window-attached composition,
/// overlay, and pacing resources.
pub(crate) struct WindowManager {
    window: Window,
    composition: CompositionTree,
    overlay: Option<EguiOverlay>,
    clock: CompositorClock,
}

impl WindowManager {
    pub fn new(
        window: Window,
        recorder: RecorderHandle,
        wake: Arc<dyn Fn(WindowCommand) -> bool + Send + Sync>,
        debug: bool,
    ) -> Result<Self, Box<dyn Error>> {
        let composition = CompositionTree::new(&window)?;
        let overlay = if debug {
            let mut overlay = EguiOverlay::new(&window, recorder)?;
            composition.set_egui_overlay_content(&overlay.content()?)?;
            overlay.draw(&window)?;
            Some(overlay)
        } else {
            None
        };
        composition.commit()?;
        Ok(Self {
            window,
            composition,
            overlay,
            clock: CompositorClock::spawn(wake)?,
        })
    }

    pub fn id(&self) -> WindowId {
        self.window.id()
    }

    pub(crate) fn native_handle(&self) -> Result<HWND, Box<dyn Error>> {
        Ok(WindowTarget::new(&self.window)?.0)
    }

    pub fn size(&self) -> winit::dpi::PhysicalSize<u32> {
        self.window.inner_size()
    }

    pub fn scale_factor(&self) -> f32 {
        self.window.scale_factor() as f32
    }

    pub fn surface(&self, width: u32, height: u32) -> SurfaceWindow<'_> {
        SurfaceWindow::new(&self.window, width, height)
    }

    pub fn compositor(&self) -> Compositor {
        self.composition.compositor.clone()
    }

    pub fn request_redraw(&self) {
        self.window.request_redraw();
    }

    pub fn set_cursor(&self, icon: Option<cursor_icon::CursorIcon>) {
        if let Some(overlay) = &self.overlay {
            overlay.set_shell_cursor(&self.window, icon);
            return;
        }
        self.window.set_cursor(icon.unwrap_or_default());
        self.window.set_cursor_visible(icon.is_some());
    }

    pub fn set_title(&self, title: &str) {
        self.window.set_title(title);
    }

    pub fn set_ime_enabled(&self, enabled: bool) {
        self.window.set_ime_allowed(enabled);
    }

    pub fn set_ime_cursor_area(&self, x: f32, y: f32, width: f32, height: f32) {
        self.window.set_ime_cursor_area(
            winit::dpi::LogicalPosition::new(x, y),
            winit::dpi::LogicalSize::new(width, height),
        );
    }

    pub fn set_shell_root(&self, root: &ContainerVisual) -> windows::core::Result<()> {
        self.composition.set_shell_root(root)
    }

    pub fn commit(&self) -> windows::core::Result<()> {
        self.composition.commit()
    }

    pub fn focus_host(&self) {
        self.composition.focus_host();
    }

    pub fn handle_window_event(&mut self, event: &WindowEvent) -> bool {
        let Some(overlay) = self.overlay.as_mut() else {
            return false;
        };
        let response = overlay.on_window_event(&self.window, event);
        if response.repaint && !matches!(event, WindowEvent::RedrawRequested) {
            self.window.request_redraw();
        }
        response.consumed
    }

    pub fn note_overlay_interaction(&mut self, now: Instant) {
        if let Some(overlay) = self.overlay.as_mut() {
            overlay.note_interaction(now);
        }
    }

    pub fn resize_overlay(&mut self) -> Result<(), Box<dyn Error>> {
        self.overlay
            .as_mut()
            .map_or(Ok(()), |overlay| overlay.resize(&self.window))
    }

    pub fn draw_overlay(&mut self) -> Result<bool, Box<dyn Error>> {
        self.overlay
            .as_mut()
            .map_or(Ok(false), |overlay| overlay.draw(&self.window))
    }

    pub fn overlay_repaint_due(&self, now: Instant) -> bool {
        self.overlay
            .as_ref()
            .is_some_and(|overlay| overlay.repaint_due(now))
    }

    pub fn next_overlay_repaint(&self) -> Option<Instant> {
        self.overlay.as_ref().map(|overlay| overlay.next_repaint())
    }

    pub fn request_overlay_repaint(&mut self) {
        if let Some(overlay) = self.overlay.as_mut() {
            overlay.request_immediate_repaint();
        }
    }

    pub fn set_overlay_note(&mut self, note: &str) {
        if let Some(overlay) = self.overlay.as_mut() {
            overlay.set_note(note);
        }
    }

    pub fn set_input_diagnostics(&mut self, diagnostics: InputDiagnostics) {
        if let Some(overlay) = self.overlay.as_mut() {
            overlay.set_input_diagnostics(diagnostics);
        }
    }

    pub fn set_backend(&mut self, backend: String) {
        if let Some(overlay) = self.overlay.as_mut() {
            overlay.set_backend(backend);
        }
    }

    pub fn add_surface(
        &mut self,
        connection: u64,
        surface: u64,
        target: u32,
        extent: polymer_renderer::Extent,
    ) {
        if let Some(overlay) = self.overlay.as_mut() {
            overlay.add_surface(connection, surface, target, extent);
        }
    }

    pub fn observe_frame_at(
        &mut self,
        connection: u64,
        frame: polymer_protocol::FrameReady,
        observed_qpc: Option<u64>,
    ) {
        if let Some(overlay) = self.overlay.as_mut() {
            overlay.observe_frame_at(connection, frame, observed_qpc);
        }
    }

    pub fn remove_target(&mut self, connection: u64) {
        if let Some(overlay) = self.overlay.as_mut() {
            overlay.remove_target(connection);
        }
    }

    pub fn record_present(&mut self, carried_new_frame: bool) {
        if let Some(overlay) = self.overlay.as_mut() {
            overlay.record_present(carried_new_frame);
        }
    }

    pub fn record_clock(&mut self) {
        if let Some(overlay) = self.overlay.as_mut() {
            overlay.record_clock();
        }
    }

    pub fn set_clock_active(&self, active: bool) {
        self.clock.set_active(active);
    }

    pub fn is_pacing(&self) -> bool {
        self.clock.is_pacing()
    }

    pub fn clear_clock_tick(&self) {
        self.clock.clear_tick();
    }
}

struct CompositionTree {
    host: WindowTarget,
    controller: CompositorController,
    compositor: Compositor,
    _target: DesktopWindowTarget,
    root: ContainerVisual,
    overlay: SpriteVisual,
}

/// Opaque native host-window target. Native handle extraction and operations
/// stay in this platform adapter instead of leaking into DOM/input code.
struct WindowTarget(HWND);

impl WindowTarget {
    fn new(window: &Window) -> Result<Self, Box<dyn Error>> {
        match window.window_handle()?.as_raw() {
            RawWindowHandle::Win32(handle) => Ok(Self(HWND(handle.hwnd.get() as *mut c_void))),
            _ => Err("Polymer composition requires a Win32 window".into()),
        }
    }

    fn focus(&self) {
        // SAFETY: CompositionTree cannot outlive the winit window used to
        // create it, and the target stores only that live host handle.
        let _ = unsafe { windows::Win32::UI::Input::KeyboardAndMouse::SetFocus(Some(self.0)) };
    }
}

impl CompositionTree {
    fn new(window: &Window) -> Result<Self, Box<dyn Error>> {
        let host = WindowTarget::new(window)?;

        let controller = CompositorController::new()?;
        let compositor = controller.Compositor()?;
        let desktop: ICompositorDesktopInterop = compositor.cast()?;
        // SAFETY: the desktop target retains the live HWND for the lifetime of
        // this tree, and WindowTarget cannot outlive its winit Window.
        let target = unsafe { desktop.CreateDesktopWindowTarget(host.0, true)? };
        let root = compositor.CreateContainerVisual()?;
        root.SetRelativeSizeAdjustment(Vector2 { X: 1.0, Y: 1.0 })?;
        target.SetRoot(&root)?;

        let overlay = compositor.CreateSpriteVisual()?;
        overlay.SetRelativeSizeAdjustment(Vector2 { X: 1.0, Y: 1.0 })?;
        root.Children()?.InsertAtTop(&overlay)?;
        controller.Commit()?;

        Ok(Self {
            host,
            controller,
            compositor,
            _target: target,
            root,
            overlay,
        })
    }

    pub fn focus_host(&self) {
        self.host.focus();
    }

    pub fn set_egui_overlay_content(&self, content: &IUnknown) -> Result<(), windows::core::Error> {
        let brush = self.surface_brush(content)?;
        self.overlay.SetBrush(&brush)
    }

    pub fn set_shell_root(&self, shell: &ContainerVisual) -> windows::core::Result<()> {
        let children = self.root.Children()?;
        children.RemoveAll()?;
        children.InsertAtTop(shell)?;
        children.InsertAtTop(&self.overlay)
    }

    fn surface_brush(
        &self,
        swapchain: &IUnknown,
    ) -> windows::core::Result<windows::UI::Composition::CompositionSurfaceBrush> {
        let interop: ICompositorInterop = self.compositor.cast()?;
        // SAFETY: the supplied object is an IDXGISwapChain created for
        // composition and retained by its renderer for at least as long as the
        // resulting surface brush remains installed.
        let surface = unsafe { interop.CreateCompositionSurfaceForSwapChain(swapchain)? };
        let brush = self.compositor.CreateSurfaceBrushWithSurface(&surface)?;
        brush.SetStretch(CompositionStretch::Fill)?;
        Ok(brush)
    }

    pub fn commit(&self) -> windows::core::Result<()> {
        self.controller.Commit()
    }
}

pub struct SurfaceWindow<'a, W: ?Sized = Window> {
    window: &'a W,
    extent: Extent,
}

impl<'a, W: ?Sized> SurfaceWindow<'a, W> {
    pub const fn new(window: &'a W, width: u32, height: u32) -> Self {
        Self {
            window,
            extent: Extent::new(width, height),
        }
    }
}

impl<W: raw_window_handle::HasWindowHandle + ?Sized> raw_window_handle::HasWindowHandle
    for SurfaceWindow<'_, W>
{
    fn window_handle(
        &self,
    ) -> Result<raw_window_handle::WindowHandle<'_>, raw_window_handle::HandleError> {
        self.window.window_handle()
    }
}

impl<W: raw_window_handle::HasDisplayHandle + ?Sized> raw_window_handle::HasDisplayHandle
    for SurfaceWindow<'_, W>
{
    fn display_handle(
        &self,
    ) -> Result<raw_window_handle::DisplayHandle<'_>, raw_window_handle::HandleError> {
        self.window.display_handle()
    }
}

impl<W: raw_window_handle::HasWindowHandle + raw_window_handle::HasDisplayHandle + ?Sized>
    polymer_renderer::RenderWindow for SurfaceWindow<'_, W>
{
    fn render_extent(&self) -> Extent {
        self.extent
    }
}
