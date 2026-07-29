#![allow(unsafe_code)]

use crate::game_layer::GameViewStyle;
use raw_window_handle::{HasWindowHandle, RawWindowHandle};
use snowglass_renderer::Extent;
use std::{collections::HashMap, error::Error, ffi::c_void};
use windows::{
    Win32::{
        Foundation::{HMODULE, HWND},
        Graphics::{
            Direct3D::D3D_DRIVER_TYPE_HARDWARE,
            Direct3D11::{D3D11_CREATE_DEVICE_BGRA_SUPPORT, D3D11_SDK_VERSION, D3D11CreateDevice},
            DirectComposition::{
                DCompositionCreateDevice, IDCompositionDevice, IDCompositionTarget,
                IDCompositionVisual,
            },
            Dxgi::IDXGIDevice,
        },
    },
    core::{IUnknown, Interface},
};
use winit::window::Window;

#[derive(Clone, Copy, Debug, Eq, Hash, PartialEq)]
pub enum SurfaceId {
    ForegroundWebView,
    EguiOverlay,
}

#[derive(Clone, Debug, Default, serde::Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct SurfaceLayout {
    #[serde(deserialize_with = "deserialize_connection")]
    pub connection: u64,
    pub x: i32,
    pub y: i32,
    pub width: u32,
    pub height: u32,
    #[serde(default)]
    pub z_index: i32,
    #[serde(default)]
    pub corner_radius: f32,
    /// GPU-only CSS properties supplied by the Blitz game layer. Foreground
    /// WebView layout messages deliberately cannot set these.
    #[serde(skip)]
    pub game_view_style: Option<GameViewStyle>,
    #[serde(skip, default = "default_opacity")]
    pub opacity: f32,
    #[serde(default = "default_pointer_events")]
    pub pointer_events: bool,
}

const fn default_pointer_events() -> bool {
    true
}

const fn default_opacity() -> f32 {
    1.0
}

#[derive(Clone, Copy, Debug, Default, serde::Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ForegroundHitRegion {
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
    merged: Option<IDCompositionVisual>,
}

/// Where a chrome band sits in the shared z-order. Bands take the even slots
/// and the game views drawn over them take the odd ones, which is what
/// interleaves the two into the document's paint order.
pub const fn chrome_band_z_index(band: usize) -> i32 {
    2 * band as i32
}

/// Where the game view drawn over `band` sits.
pub const fn game_view_z_index(band: usize) -> i32 {
    chrome_band_z_index(band) + 1
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
            // Game surfaces and chrome are already merged by D3D12. This
            // device only hosts that swapchain below WebView2.
            let mut d3d11_device = None;
            D3D11CreateDevice(
                None,
                D3D_DRIVER_TYPE_HARDWARE,
                HMODULE::default(),
                D3D11_CREATE_DEVICE_BGRA_SUPPORT,
                None,
                D3D11_SDK_VERSION,
                Some(&mut d3d11_device),
                None,
                None,
            )?;
            let d3d11_device = d3d11_device.ok_or("D3D11 returned no device")?;
            let dxgi_device: IDXGIDevice = d3d11_device.cast()?;
            let device: IDCompositionDevice = DCompositionCreateDevice(&dxgi_device)?;
            let target = device.CreateTargetForHwnd(hwnd, true)?;
            let root = device.CreateVisual()?;
            let foreground_webview = device.CreateVisual()?;
            let egui_overlay = device.CreateVisual()?;
            // The game layer's chrome and the streams are inserted beneath
            // these two as they appear, so the foreground page and the debug
            // overlay always stay on top.
            root.AddVisual(&foreground_webview, true, None::<&IDCompositionVisual>)?;
            root.AddVisual(&egui_overlay, true, &foreground_webview)?;
            target.SetRoot(&root)?;
            device.Commit()?;

            let mut surfaces = HashMap::new();
            surfaces.insert(SurfaceId::ForegroundWebView, foreground_webview);
            surfaces.insert(SurfaceId::EguiOverlay, egui_overlay);
            Ok(Self {
                device,
                _target: target,
                root,
                surfaces,
                merged: None,
            })
        }
    }

    pub fn foreground_visual(&self) -> Result<IUnknown, windows::core::Error> {
        self.surfaces[&SurfaceId::ForegroundWebView].cast()
    }

    pub fn set_egui_overlay_content(&self, content: &IUnknown) -> Result<(), windows::core::Error> {
        // SAFETY: the content is a composition swapchain retained by the
        // shell's overlay compositor.
        unsafe { self.surfaces[&SurfaceId::EguiOverlay].SetContent(content) }
    }

    /// Attaches the one window-spanning game swapchain below WebView2.
    pub fn set_merged_content(&mut self, content: &IUnknown) -> Result<(), windows::core::Error> {
        // SAFETY: the content is a composition swapchain, and the visual and
        // content remain retained by the tree and merged compositor.
        unsafe {
            let visual = self.device.CreateVisual()?;
            visual.SetContent(content)?;
            // With no reference visual, `insert_above = true` inserts at the
            // bottom of the child list, beneath WebView2 and the egui overlay.
            self.root
                .AddVisual(&visual, true, None::<&IDCompositionVisual>)?;
            if let Some(previous) = self.merged.replace(visual) {
                self.root.RemoveVisual(&previous)?;
            }
        }
        Ok(())
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
    use super::{ForegroundHitRegion, SurfaceLayout};

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

    #[test]
    fn foreground_hit_regions_deserialize_independently_of_surface_layout() {
        let region: ForegroundHitRegion =
            serde_json::from_str(r#"{"x": 400, "y": 300, "width": 716, "height": 380}"#).unwrap();

        assert_eq!((region.x, region.y), (400, 300));
        assert_eq!((region.width, region.height), (716, 380));
    }
}
