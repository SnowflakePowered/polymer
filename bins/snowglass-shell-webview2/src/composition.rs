#![allow(unsafe_code)]

use raw_window_handle::{HasWindowHandle, RawWindowHandle};
use snowglass_renderer::Extent;
use std::{collections::HashMap, error::Error, ffi::c_void};
use windows::{
    Win32::{
        Foundation::HWND,
        Graphics::{
            DirectComposition::{
                DCOMPOSITION_BITMAP_INTERPOLATION_MODE_INHERIT,
                DCOMPOSITION_BITMAP_INTERPOLATION_MODE_NEAREST_NEIGHBOR, DCompositionCreateDevice,
                IDCompositionDevice, IDCompositionScaleTransform, IDCompositionTarget,
                IDCompositionTransform, IDCompositionVisual,
            },
            Dxgi::IDXGIDevice,
        },
    },
    core::{IUnknown, Interface},
};
use winit::window::Window;

#[derive(Clone, Copy, Debug, Eq, Hash, PartialEq)]
pub enum SurfaceId {
    BackgroundWebView,
    ForegroundWebView,
    EguiOverlay,
}

#[derive(Clone, Copy, Debug, Eq, Hash, PartialEq)]
pub enum WebViewLayer {
    Background,
    Foreground,
}

#[derive(Clone, Copy, Debug, Default, serde::Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct SurfaceLayout {
    #[serde(deserialize_with = "deserialize_connection")]
    pub connection: u64,
    pub x: i32,
    pub y: i32,
    pub width: u32,
    pub height: u32,
}

fn deserialize_connection<'de, D>(deserializer: D) -> Result<u64, D::Error>
where
    D: serde::Deserializer<'de>,
{
    #[derive(serde::Deserialize)]
    #[serde(untagged)]
    enum Connection {
        Number(u64),
        String(String),
    }

    match <Connection as serde::Deserialize>::deserialize(deserializer)? {
        Connection::Number(connection) => Ok(connection),
        Connection::String(connection) => connection.parse().map_err(serde::de::Error::custom),
    }
}

pub struct CompositionTree {
    device: IDCompositionDevice,
    _target: IDCompositionTarget,
    root: IDCompositionVisual,
    surfaces: HashMap<SurfaceId, IDCompositionVisual>,
    render_targets: HashMap<u64, RenderTargetVisual>,
}

struct RenderTargetVisual {
    visual: IDCompositionVisual,
    scale: IDCompositionScaleTransform,
}

impl CompositionTree {
    pub fn new(window: &Window) -> Result<Self, Box<dyn Error>> {
        let hwnd = match window.window_handle()?.as_raw() {
            RawWindowHandle::Win32(handle) => HWND(handle.hwnd.get() as *mut c_void),
            _ => return Err("WebView2 composition requires a Win32 window".into()),
        };

        // SAFETY: the DirectComposition device, target, and visuals are
        // retained by this tree for at least as long as the host HWND.
        unsafe {
            let device: IDCompositionDevice = DCompositionCreateDevice(None::<&IDXGIDevice>)?;
            let target = device.CreateTargetForHwnd(hwnd, true)?;
            let root = device.CreateVisual()?;
            let background_webview = device.CreateVisual()?;
            let foreground_webview = device.CreateVisual()?;
            let egui_overlay = device.CreateVisual()?;
            root.AddVisual(&background_webview, true, None::<&IDCompositionVisual>)?;
            root.AddVisual(&foreground_webview, true, &background_webview)?;
            root.AddVisual(&egui_overlay, true, &foreground_webview)?;
            target.SetRoot(&root)?;
            device.Commit()?;

            let mut surfaces = HashMap::new();
            surfaces.insert(SurfaceId::BackgroundWebView, background_webview);
            surfaces.insert(SurfaceId::ForegroundWebView, foreground_webview);
            surfaces.insert(SurfaceId::EguiOverlay, egui_overlay);
            Ok(Self {
                device,
                _target: target,
                root,
                surfaces,
                render_targets: HashMap::new(),
            })
        }
    }

    pub fn webview_visual(&self, layer: WebViewLayer) -> Result<IUnknown, windows::core::Error> {
        let id = match layer {
            WebViewLayer::Background => SurfaceId::BackgroundWebView,
            WebViewLayer::Foreground => SurfaceId::ForegroundWebView,
        };
        self.surfaces[&id].cast()
    }

    pub fn set_egui_overlay_content(&self, content: &IUnknown) -> Result<(), windows::core::Error> {
        // SAFETY: the content is a composition swapchain retained by the
        // shell's overlay compositor.
        unsafe { self.surfaces[&SurfaceId::EguiOverlay].SetContent(content) }
    }

    pub fn add_render_target(
        &mut self,
        connection: u64,
        content: &IUnknown,
    ) -> Result<(), windows::core::Error> {
        self.remove_render_target(connection)?;
        // SAFETY: the content is a composition swapchain, and the visual and
        // content remain retained by the tree and its stream compositor.
        unsafe {
            let visual = self.device.CreateVisual()?;
            let scale = self.device.CreateScaleTransform()?;
            visual.SetContent(content)?;
            let background = &self.surfaces[&SurfaceId::BackgroundWebView];
            self.root.AddVisual(&visual, true, background)?;
            self.render_targets
                .insert(connection, RenderTargetVisual { visual, scale });
            Ok(())
        }
    }

    pub fn remove_render_target(&mut self, connection: u64) -> windows::core::Result<()> {
        let Some(target) = self.render_targets.remove(&connection) else {
            return Ok(());
        };
        // SAFETY: `visual` is a direct child of `root` while present in the
        // surface map.
        unsafe {
            self.root.RemoveVisual(&target.visual)?;
            self.device.Commit()
        }
    }

    pub fn apply_layout(
        &self,
        layout: SurfaceLayout,
        content_width: u32,
        content_height: u32,
    ) -> windows::core::Result<()> {
        let Some(target) = self.render_targets.get(&layout.connection) else {
            return Ok(());
        };
        // DirectComposition coordinates are physical pixels. Zero-sized DOM
        // boxes are hidden without resizing their swapchains to zero. Scaling
        // lets visuals follow live DOM layout immediately while their
        // swapchains are resized only after the window resize settles.
        unsafe {
            let (x, y) = if layout.width == 0 || layout.height == 0 {
                (-100_000.0, -100_000.0)
            } else {
                (layout.x as f32, layout.y as f32)
            };
            target.visual.SetOffsetX2(x)?;
            target.visual.SetOffsetY2(y)?;
            let needs_scale = layout.width != 0
                && layout.height != 0
                && (layout.width != content_width || layout.height != content_height);
            if needs_scale {
                target
                    .scale
                    .SetScaleX2(layout.width as f32 / content_width.max(1) as f32)?;
                target
                    .scale
                    .SetScaleY2(layout.height as f32 / content_height.max(1) as f32)?;
                target.visual.SetBitmapInterpolationMode(
                    DCOMPOSITION_BITMAP_INTERPOLATION_MODE_NEAREST_NEIGHBOR,
                )?;
                target.visual.SetTransform(&target.scale)?;
            } else {
                // A null transform leaves the settled visual in the simplest
                // form for DWM's independent-flip/MPO eligibility checks.
                target
                    .visual
                    .SetTransform(None::<&IDCompositionTransform>)?;
                target
                    .visual
                    .SetBitmapInterpolationMode(DCOMPOSITION_BITMAP_INTERPOLATION_MODE_INHERIT)?;
            }
            Ok(())
        }
    }

    pub fn commit(&self) -> windows::core::Result<()> {
        // SAFETY: commits the retained tree without borrowing external data.
        unsafe { self.device.Commit() }
    }
}

pub struct SurfaceWindow<'a> {
    window: &'a Window,
    extent: Extent,
}

impl<'a> SurfaceWindow<'a> {
    pub const fn new(window: &'a Window, width: u32, height: u32) -> Self {
        Self {
            window,
            extent: Extent::new(width, height),
        }
    }
}

impl raw_window_handle::HasWindowHandle for SurfaceWindow<'_> {
    fn window_handle(
        &self,
    ) -> Result<raw_window_handle::WindowHandle<'_>, raw_window_handle::HandleError> {
        self.window.window_handle()
    }
}

impl raw_window_handle::HasDisplayHandle for SurfaceWindow<'_> {
    fn display_handle(
        &self,
    ) -> Result<raw_window_handle::DisplayHandle<'_>, raw_window_handle::HandleError> {
        self.window.display_handle()
    }
}

impl snowglass_renderer::RenderWindow for SurfaceWindow<'_> {
    fn render_extent(&self) -> Extent {
        self.extent
    }
}

#[cfg(test)]
mod tests {
    use super::SurfaceLayout;

    #[test]
    fn layout_preserves_connection_ids_larger_than_javascript_safe_integers() {
        let layout: SurfaceLayout = serde_json::from_str(
            r#"{
                "connection": "6000765028494475265",
                "x": 10,
                "y": 20,
                "width": 854,
                "height": 480
            }"#,
        )
        .unwrap();

        assert_eq!(layout.connection, 6_000_765_028_494_475_265);
    }

    #[test]
    fn layout_still_accepts_numeric_connection_ids() {
        let layout: SurfaceLayout =
            serde_json::from_str(r#"{"connection": 42, "x": 0, "y": 0, "width": 1, "height": 1}"#)
                .unwrap();

        assert_eq!(layout.connection, 42);
    }
}
