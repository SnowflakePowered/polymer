#![allow(unsafe_code)]

use raw_window_handle::{HasWindowHandle, RawWindowHandle};
use snowglass_renderer::Extent;
use std::{collections::HashMap, error::Error, ffi::c_void};
use windows::{
    Win32::{
        Foundation::{HANDLE, HMODULE, HWND, POINT},
        Graphics::{
            Direct3D::D3D_DRIVER_TYPE_HARDWARE,
            Direct3D11::{
                D3D11_CREATE_DEVICE_BGRA_SUPPORT, D3D11_SDK_VERSION, D3D11CreateDevice,
                ID3D11Device, ID3D11Device1, ID3D11DeviceContext, ID3D11Texture2D,
            },
            DirectComposition::{
                DCOMPOSITION_BITMAP_INTERPOLATION_MODE_INHERIT,
                DCOMPOSITION_BITMAP_INTERPOLATION_MODE_NEAREST_NEIGHBOR, DCompositionCreateDevice,
                IDCompositionDevice, IDCompositionScaleTransform, IDCompositionSurface,
                IDCompositionTarget, IDCompositionTransform, IDCompositionVisual,
            },
            Dxgi::{
                Common::{DXGI_ALPHA_MODE_PREMULTIPLIED, DXGI_FORMAT_R8G8B8A8_UNORM},
                IDXGIDevice,
            },
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
    #[serde(default = "default_pointer_events")]
    pub pointer_events: bool,
    #[serde(default)]
    pub foreground_hit_region: Option<ForegroundHitRegion>,
    #[serde(default)]
    pub foreground_hit_regions: Vec<ForegroundHitRegion>,
}

const fn default_pointer_events() -> bool {
    true
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
    /// Backs the chrome surfaces. DirectComposition hands out textures owned by
    /// whichever device it was created over, so uploading CPU pixels into them
    /// means holding that device here.
    chrome_device: ID3D11Device,
    chrome_context: ID3D11DeviceContext,
    chrome_bands: Vec<ChromeBand>,
    _target: IDCompositionTarget,
    root: IDCompositionVisual,
    surfaces: HashMap<SurfaceId, IDCompositionVisual>,
    render_targets: HashMap<u64, RenderTargetVisual>,
}

struct RenderTargetVisual {
    index: u32,
    z_index: i32,
    visual: IDCompositionVisual,
    scale: IDCompositionScaleTransform,
}

/// One slice of the game layer's chrome, drawn between two game views.
struct ChromeBand {
    visual: IDCompositionVisual,
    surface: Option<IDCompositionSurface>,
    /// The shared texture the band was rasterised into, kept open so a repaint
    /// of the same band is only a copy. Paired with the handle it came from so
    /// a new one is noticed.
    source: Option<(isize, ID3D11Texture2D)>,
    width: u32,
    height: u32,
    /// Whether the band is in the visual tree. An empty band is left out so it
    /// costs DWM nothing.
    attached: bool,
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
            // The game views present through their own D3D12 swapchains and are
            // untouched by this device; it exists only so the chrome bands have
            // surfaces that can be written from the CPU.
            let mut chrome_device = None;
            let mut chrome_context = None;
            D3D11CreateDevice(
                None,
                D3D_DRIVER_TYPE_HARDWARE,
                HMODULE::default(),
                D3D11_CREATE_DEVICE_BGRA_SUPPORT,
                None,
                D3D11_SDK_VERSION,
                Some(&mut chrome_device),
                None,
                Some(&mut chrome_context),
            )?;
            let chrome_device = chrome_device.ok_or("D3D11 returned no device")?;
            let chrome_context = chrome_context.ok_or("D3D11 returned no device context")?;
            let dxgi_device: IDXGIDevice = chrome_device.cast()?;
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
                chrome_device,
                chrome_context,
                chrome_bands: Vec::new(),
                _target: target,
                root,
                surfaces,
                render_targets: HashMap::new(),
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

    pub fn add_render_target(
        &mut self,
        connection: u64,
        index: u32,
        content: &IUnknown,
    ) -> Result<(), windows::core::Error> {
        self.remove_render_target(connection)?;
        // SAFETY: the content is a composition swapchain, and the visual and
        // content remain retained by the tree and its stream compositor.
        unsafe {
            let visual = self.device.CreateVisual()?;
            let scale = self.device.CreateScaleTransform()?;
            visual.SetContent(content)?;
            self.render_targets.insert(
                connection,
                RenderTargetVisual {
                    index,
                    z_index: index as i32,
                    visual,
                    scale,
                },
            );
        }
        // The new visual has to be stacked against the chrome bands as well as
        // the other targets, so the whole span is rebuilt rather than spliced.
        self.reorder_layers()
    }

    pub fn set_render_target_z_index(
        &mut self,
        connection: u64,
        z_index: i32,
    ) -> windows::core::Result<()> {
        let Some(target) = self.render_targets.get_mut(&connection) else {
            return Ok(());
        };
        if target.z_index == z_index {
            return Ok(());
        }
        target.z_index = z_index;
        self.reorder_layers()
    }

    /// Replaces the chrome painted between the game views.
    ///
    /// `handle` is a Win32 shared handle to a premultiplied RGBA texture the
    /// shell rasterised on the GPU. Bands are numbered from the bottom; band
    /// `n` is drawn below game view `n`.
    ///
    /// The caller must have finished writing the texture before calling: the
    /// copy below is issued straight away.
    pub fn set_chrome_band(
        &mut self,
        band: usize,
        handle: isize,
        width: u32,
        height: u32,
    ) -> windows::core::Result<()> {
        if width == 0 || height == 0 || handle == 0 {
            return Ok(());
        }
        while self.chrome_bands.len() <= band {
            // SAFETY: the visual is retained by the band list from here on.
            let visual = unsafe { self.device.CreateVisual()? };
            self.chrome_bands.push(ChromeBand {
                visual,
                surface: None,
                source: None,
                width: 0,
                height: 0,
                attached: false,
            });
        }
        // SAFETY: every interface below is retained by this tree, and the
        // pixels outlive the copy that reads them.
        unsafe {
            let entry = &mut self.chrome_bands[band];
            if entry.surface.is_none() || (entry.width, entry.height) != (width, height) {
                let surface = self.device.CreateSurface(
                    width,
                    height,
                    DXGI_FORMAT_R8G8B8A8_UNORM,
                    DXGI_ALPHA_MODE_PREMULTIPLIED,
                )?;
                entry.visual.SetContent(&surface)?;
                entry.surface = Some(surface);
                entry.width = width;
                entry.height = height;
            }
            let surface = entry.surface.as_ref().expect("just created");

            // The band was rasterised on the GPU, so it reaches the surface as
            // a copy from the texture it was drawn into rather than a trip
            // through system memory. Opening the handle once per band is what
            // keeps this to a copy.
            if entry.source.as_ref().is_none_or(|held| held.0 != handle) {
                let opened: ID3D11Texture2D = self
                    .chrome_device
                    .cast::<ID3D11Device1>()?
                    .OpenSharedResource1(HANDLE(handle as *mut c_void))?;
                entry.source = Some((handle, opened));
            }
            let source = entry
                .source
                .as_ref()
                .map(|held| held.1.clone())
                .expect("just opened");

            // DirectComposition may hand back a tile inside a larger atlas, so
            // the copy has to land at the offset it reports.
            let mut offset = POINT::default();
            let destination: ID3D11Texture2D = surface.BeginDraw(None, &mut offset)?;
            self.chrome_context.CopySubresourceRegion(
                &destination,
                0,
                offset.x as u32,
                offset.y as u32,
                0,
                &source,
                0,
                None,
            );
            surface.EndDraw()?;
        }
        self.attach_chrome_band(band, true)
    }

    /// Drops the bands from `count` upwards, which is how a layer that stopped
    /// drawing above its last game view stops paying for the empty band.
    pub fn truncate_chrome_bands(&mut self, count: usize) -> windows::core::Result<()> {
        for band in count..self.chrome_bands.len() {
            self.attach_chrome_band(band, false)?;
        }
        Ok(())
    }

    fn attach_chrome_band(&mut self, band: usize, attached: bool) -> windows::core::Result<()> {
        let entry = &mut self.chrome_bands[band];
        if entry.attached == attached {
            return Ok(());
        }
        entry.attached = attached;
        if !attached {
            // SAFETY: an attached band is a direct child of `root`.
            unsafe { self.root.RemoveVisual(&entry.visual)? };
            return Ok(());
        }
        self.reorder_layers()
    }

    /// Restacks everything below the foreground WebView.
    ///
    /// The chrome bands and the game views share one z-order because the
    /// document interleaves them: chrome can be painted both under a game view
    /// and over it, as a handheld bezel is when it sits on a fullscreen view.
    fn reorder_layers(&self) -> windows::core::Result<()> {
        enum Layer<'a> {
            Band(&'a IDCompositionVisual),
            Target(&'a RenderTargetVisual),
        }

        let mut layers = self
            .chrome_bands
            .iter()
            .enumerate()
            .filter(|(_, band)| band.attached)
            .map(|(index, band)| (chrome_band_z_index(index), 0u32, Layer::Band(&band.visual)))
            .chain(
                self.render_targets
                    .values()
                    .map(|target| (target.z_index, target.index, Layer::Target(target))),
            )
            .collect::<Vec<_>>();
        layers.sort_by_key(|(z_index, tiebreak, _)| (*z_index, *tiebreak));

        // SAFETY: every visual listed is either a direct child of `root` or
        // about to become one, and all are retained by this tree.
        unsafe {
            for (_, _, layer) in &layers {
                let visual = match layer {
                    Layer::Band(visual) => visual,
                    Layer::Target(target) => &target.visual,
                };
                // A visual that is not currently a child is simply not found,
                // which `RemoveVisual` reports rather than treating as fatal.
                let _ = self.root.RemoveVisual(visual);
            }
            // The first goes at the very bottom of the tree; the rest stack on
            // top of it, which leaves the whole run below the foreground.
            let mut below: Option<&IDCompositionVisual> = None;
            for (_, _, layer) in &layers {
                let visual = match layer {
                    Layer::Band(visual) => *visual,
                    Layer::Target(target) => &target.visual,
                };
                match below {
                    Some(reference) => self.root.AddVisual(visual, true, reference)?,
                    None => self
                        .root
                        .AddVisual(visual, true, None::<&IDCompositionVisual>)?,
                }
                below = Some(visual);
            }
        }
        Ok(())
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
        layout: &SurfaceLayout,
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
                "height": 480,
                "foregroundHitRegion": {
                    "x": 400,
                    "y": 300,
                    "width": 716,
                    "height": 380
                },
                "foregroundHitRegions": [{
                    "x": 10,
                    "y": 20,
                    "width": 100,
                    "height": 40
                }, {
                    "x": 10,
                    "y": 200,
                    "width": 100,
                    "height": 40
                }]
            }"#,
        )
        .unwrap();

        assert_eq!(layout.connection, 6_000_765_028_494_475_265);
        let hit_region = layout.foreground_hit_region.unwrap();
        assert_eq!((hit_region.x, hit_region.y), (400, 300));
        assert_eq!((hit_region.width, hit_region.height), (716, 380));
        assert_eq!(layout.foreground_hit_regions.len(), 2);
        assert_eq!(
            (
                layout.foreground_hit_regions[1].x,
                layout.foreground_hit_regions[1].y
            ),
            (10, 200)
        );
    }

    #[test]
    fn layout_still_accepts_numeric_connection_ids() {
        let layout: SurfaceLayout =
            serde_json::from_str(r#"{"connection": 42, "x": 0, "y": 0, "width": 1, "height": 1}"#)
                .unwrap();

        assert_eq!(layout.connection, 42);
    }
}
