use crate::{raster::RasterLayer, surfaces::Surfaces};
use objc2::{MainThreadMarker, rc::Retained, runtime::AnyObject};
use objc2_app_kit::NSView;
use objc2_core_image::CIFilter;
use objc2_foundation::{
    NSArray, NSNumber, NSObjectNSKeyValueCoding, NSPoint as CGPoint, NSRect as CGRect,
    NSSize as CGSize, ns_string,
};
use objc2_quartz_core::{CALayer, CATransaction};
use polymer_composition::{CompositionScene, RasterChunkId, ShellCompositionGroup, SurfaceLayer};
use raw_window_handle::{HasWindowHandle, RawWindowHandle};
use std::{
    collections::{HashMap, HashSet},
    error::Error,
};

struct Group {
    outer: Retained<CALayer>,
    content: Retained<CALayer>,
}
impl Group {
    fn new() -> Self {
        let outer = CALayer::new();
        let content = CALayer::new();
        content.setMasksToBounds(true);
        outer.addSublayer(&content);
        Self { outer, content }
    }
    fn update(&self, group: &ShellCompositionGroup, scale: f64) {
        self.outer.setOpacity(group.opacity.clamp(0., 1.));
        LayerTree::place(
            &self.content,
            &self.outer,
            [0., 0., group.bounds.width(), group.bounds.height()],
            0,
            scale,
        );
        if let Some(shadow) = group.shadow {
            let [r, g, b, a] = shadow.color.map(|v| v as f64 / 255.);
            let color = objc2_core_graphics::CGColor::new_generic_rgb(r, g, b, a);
            self.outer.setShadowColor(Some(&color));
            self.outer.setShadowOpacity(1.);
            self.outer
                .setShadowRadius(shadow.blur_radius.max(0.) as f64 / scale);
            self.outer.setShadowOffset(CGSize::new(
                shadow.offset[0] as f64 / scale,
                shadow.offset[1] as f64 / scale,
            ));
        } else {
            self.outer.setShadowOpacity(0.);
        }
        let filters = group.backdrop_blur.and_then(|blur| {
            let radius = NSNumber::new_f64(f64::from(blur) / scale);
            let radius: &AnyObject = &radius;
            // SAFETY: CIGaussianBlur is a built-in filter whose `inputRadius`
            // takes an NSNumber; macOS leaves inputs undefined until defaulted.
            let filter = unsafe {
                let filter = CIFilter::filterWithName(ns_string!("CIGaussianBlur"))?;
                filter.setDefaults();
                filter.setValue_forKey(Some(radius), ns_string!("inputRadius"));
                filter
            };
            let object: &AnyObject = &filter;
            Some(NSArray::from_slice(&[object]))
        });
        // SAFETY: the array holds only CIFilter instances.
        unsafe { self.outer.setBackgroundFilters(filters.as_deref()) };
    }
}
impl Drop for Group {
    fn drop(&mut self) {
        self.outer.removeFromSuperlayer();
    }
}

pub(crate) struct LayerTree {
    pub root: Retained<CALayer>,
    groups: HashMap<u64, Group>,
}
impl LayerTree {
    pub fn new(window: &winit::window::Window) -> Result<Self, Box<dyn Error>> {
        MainThreadMarker::new().ok_or("composition must be constructed on the AppKit thread")?;
        let RawWindowHandle::AppKit(handle) = window.window_handle()?.as_raw() else {
            return Err("composition requires an AppKit view".into());
        };
        // winit owns this NSView for the lifetime of the backend's Window.
        let view = unsafe { &*handle.ns_view.as_ptr().cast::<NSView>() };
        let root = CALayer::new();
        root.setGeometryFlipped(true);
        view.setWantsLayer(true);
        view.setLayer(Some(&root));
        view.setLayerUsesCoreImageFilters(true);
        Ok(Self {
            root,
            groups: HashMap::new(),
        })
    }
    pub fn validate(scene: &CompositionScene) -> Result<(), Box<dyn Error>> {
        let parents = scene
            .composition_groups
            .iter()
            .map(|g| (g.id, g.parent))
            .collect::<HashMap<_, _>>();
        if parents.len() != scene.composition_groups.len() {
            return Err("duplicate composition group".into());
        }
        for &id in parents.keys() {
            let mut seen = HashSet::new();
            let mut next = Some(id);
            while let Some(id) = next {
                if !seen.insert(id) {
                    return Err("cyclic composition group".into());
                }
                next = *parents.get(&id).ok_or("missing composition parent")?;
            }
        }
        Ok(())
    }
    pub fn reconcile(
        &mut self,
        scene: Option<&CompositionScene>,
        rasters: &HashMap<RasterChunkId, RasterLayer>,
        surfaces: &[SurfaceLayer],
        streams: &Surfaces,
        scale: f64,
    ) {
        let groups = scene.map_or(&[][..], |scene| scene.composition_groups.as_slice());
        CATransaction::begin();
        CATransaction::setDisableActions(true);
        self.update_groups(groups, scale);
        if let Some(scene) = scene {
            self.update_rasters(scene, rasters, scale);
        }
        self.update_surfaces(groups, surfaces, streams, scale);
        CATransaction::commit();
    }
    fn parent<'a>(
        &'a self,
        id: Option<u64>,
        groups: &[ShellCompositionGroup],
    ) -> (&'a CALayer, [f64; 2]) {
        let layer = id
            .and_then(|id| self.groups.get(&id))
            .map_or(&*self.root, |g| &*g.content);
        let origin = id
            .and_then(|id| groups.iter().find(|g| g.id == id))
            .map_or([0., 0.], |g| [g.bounds.x0, g.bounds.y0]);
        (layer, origin)
    }
    fn update_groups(&mut self, groups: &[ShellCompositionGroup], scale: f64) {
        for group in groups {
            self.groups.entry(group.id).or_insert_with(Group::new);
        }
        // Detach first so reparenting a previously nested group cannot temporarily
        // create a cycle while applying a valid new hierarchy.
        for group in self.groups.values() {
            group.outer.removeFromSuperlayer();
        }
        self.groups
            .retain(|id, _| groups.iter().any(|g| g.id == *id));
        for group in groups {
            let retained = &self.groups[&group.id];
            let (parent, origin) = self.parent(group.parent, groups);
            Self::place(
                &retained.outer,
                parent,
                [
                    group.bounds.x0 - origin[0],
                    group.bounds.y0 - origin[1],
                    group.bounds.width(),
                    group.bounds.height(),
                ],
                group.z_order,
                scale,
            );
            retained.update(group, scale);
        }
    }
    fn update_rasters(
        &self,
        scene: &CompositionScene,
        rasters: &HashMap<RasterChunkId, RasterLayer>,
        scale: f64,
    ) {
        for chunk in &scene.raster_chunks {
            let Some(raster) = rasters.get(&chunk.id) else {
                continue;
            };
            let (parent, origin) = self.parent(chunk.group, &scene.composition_groups);
            Self::place(
                raster.renderer.layer(),
                parent,
                [
                    chunk.bounds.x0 - origin[0],
                    chunk.bounds.y0 - origin[1],
                    chunk.bounds.width(),
                    chunk.bounds.height(),
                ],
                chunk.z_order,
                scale,
            );
        }
    }
    fn update_surfaces(
        &self,
        groups: &[ShellCompositionGroup],
        surfaces: &[SurfaceLayer],
        streams: &Surfaces,
        scale: f64,
    ) {
        for (connection, stream) in &streams.streams {
            let layer = stream.renderer.layer();
            let Some(surface) = surfaces.iter().find(|s| s.connection == *connection) else {
                layer.removeFromSuperlayer();
                continue;
            };
            let (parent, origin) = self.parent(surface.parent, groups);
            let [x, y, right, bottom] = surface.placement.destination.map(f64::from);
            Self::place(
                layer,
                parent,
                [x - origin[0], y - origin[1], right - x, bottom - y],
                surface.z_order,
                scale,
            );
            layer.setOpacity(surface.placement.opacity.clamp(0., 1.));
            let [u, v, r, b] = surface.placement.source.map(f64::from);
            layer.setContentsRect(CGRect::new(CGPoint::new(u, v), CGSize::new(r - u, b - v)));
        }
    }
    pub fn place(layer: &CALayer, parent: &CALayer, rect: [f64; 4], z: i32, scale: f64) {
        parent.addSublayer(layer);
        layer.setAnchorPoint(CGPoint::new(0., 0.));
        layer.setPosition(CGPoint::new(rect[0] / scale, rect[1] / scale));
        layer.setBounds(CGRect::new(
            CGPoint::new(0., 0.),
            CGSize::new(rect[2].max(0.) / scale, rect[3].max(0.) / scale),
        ));
        layer.setContentsScale(scale);
        layer.setZPosition(z as f64);
    }
    pub fn commit(&self) {
        CATransaction::flush();
    }
}
