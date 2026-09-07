//! Presentation dependency-graph tracking.
//!
//! Persistent, generation-stamped observations of which images are rendered
//! into, what they were filled from, and what reached the swapchain. The
//! graph is what lets a capture pick the offscreen screens a compositor
//! samples instead of the composite it blits to the swapchain. The hook
//! layer forwards raw command observations; selection policy lives in polymer-presentation,
//! and each selected source is expected to become its own capture stream —
//! composition is never this crate's concern.

mod commands;
mod selection;
mod surfaces;

use ash::vk;
use dashmap::DashMap;
use std::{
    collections::HashMap,
    sync::atomic::{AtomicU64, Ordering},
};

/// Device-scoped state shared by the Vulkan hook contexts.
#[derive(Default)]
pub struct Tracking {
    command_dependencies: DashMap<vk::CommandBuffer, Vec<vk::CommandBuffer>>,
    command_pools: DashMap<vk::CommandBuffer, vk::CommandPool>,
    surfaces: surfaces::Surfaces,
    command_observations: DashMap<vk::CommandBuffer, Vec<commands::RecordedCommand>>,
    image_extent: DashMap<vk::Image, vk::Extent2D>,
    image_info: DashMap<vk::Image, (vk::Format, vk::ImageUsageFlags)>,
    image_view: DashMap<vk::ImageView, Vec<surfaces::SurfaceId>>,
    color_image_view: DashMap<vk::ImageView, Vec<surfaces::SurfaceId>>,
    framebuffer_images: DashMap<vk::Framebuffer, Vec<surfaces::SurfaceId>>,
    descriptor_set_images: DashMap<
        vk::DescriptorSet,
        HashMap<(u32, u32), (Vec<surfaces::SurfaceId>, vk::ImageLayout)>,
    >,
    command_tracking: DashMap<vk::CommandBuffer, CommandTrackingState>,
    /// Presentation images and their bounds. Keep these separate from
    /// `image_extent`: inserting a swapchain image into the graph's general
    /// image table makes presentation compositor draws participate in source
    /// selection and destabilizes the offscreen targets selected for capture.
    swapchain_images: DashMap<vk::Image, vk::Extent2D>,
}

/// One selected capture source: a whole presented screen.
#[derive(Clone, Copy)]
pub struct PresentSource {
    pub logical_slot: Option<usize>,
    pub subresource: (u32, u32),
    pub incarnation: u64,
    pub selection: polymer_presentation::SurfaceSelection,
    pub allocation_extent: vk::Extent2D,
    pub image: vk::Image,
    pub layout: vk::ImageLayout,
}

impl polymer_presentation::history::HistorySurface for PresentSource {
    fn selection(&mut self) -> &mut polymer_presentation::SurfaceSelection {
        &mut self.selection
    }
    type Id = (vk::Image, u64, u32, u32);
    type Layout = (u32, u32, u32, u32);
    fn identity(&self) -> Self::Id {
        (
            self.image,
            self.incarnation,
            self.subresource.0,
            self.subresource.1,
        )
    }
    fn layout(&self) -> Self::Layout {
        (
            self.selection.source_rect.extent.width,
            self.selection.source_rect.extent.height,
            self.subresource.0,
            self.subresource.1,
        )
    }
}

impl Tracking {
    fn generation(&self) -> u64 {
        self.surfaces.observations.generation()
    }

    pub fn note_image(&self, image: vk::Image, extent: vk::Extent2D) {
        self.surface_id(image, 0, 0);
        self.image_extent.insert(image, extent);
    }

    pub fn image_extent(&self, image: vk::Image) -> Option<vk::Extent2D> {
        self.image_extent.get(&image).map(|entry| *entry)
    }

    pub fn forget_image(&self, image: vk::Image) {
        self.forget_surfaces(image);
        self.image_extent.remove(&image);
    }

    #[cfg(test)]
    pub fn refresh_sources(&self, sources: &[PresentSource]) -> Vec<PresentSource> {
        sources
            .iter()
            .filter_map(|source| self.refresh_source(source))
            .collect()
    }

    pub fn refresh_source(&self, source: &PresentSource) -> Option<PresentSource> {
        let id = surfaces::SurfaceId {
            image: source.image,
            incarnation: source.incarnation,
            mip: source.subresource.0,
            layer: source.subresource.1,
        };
        let extent = self.surface_extent(id)?;
        if extent.width != source.allocation_extent.width
            || extent.height != source.allocation_extent.height
        {
            return None;
        }
        let x = u32::try_from(source.selection.source_rect.x).ok()?;
        let y = u32::try_from(source.selection.source_rect.y).ok()?;
        if x.checked_add(source.selection.source_rect.extent.width)? > extent.width
            || y.checked_add(source.selection.source_rect.extent.height)? > extent.height
        {
            return None;
        }
        let layout = self
            .surfaces
            .layouts
            .get(&id)
            .map(|value| *value)
            .unwrap_or(source.layout);
        if matches!(
            layout,
            vk::ImageLayout::UNDEFINED | vk::ImageLayout::PREINITIALIZED
        ) {
            return None;
        }
        Some(PresentSource { layout, ..*source })
    }

    /// Clears the accumulated dependency graph and render-target history so the
    /// next presents rebuild it from scratch. A live render-target
    /// reallocation (a resolution switch destroys the offscreen screens and
    /// creates new ones) otherwise leaves the incremental selection carrying
    /// stale edges from the old layout and re-resolving only part of the new
    /// one, where a from-empty rebuild recovers every screen. The descriptive
    /// stores (`image_extent`, `image_info`, view and framebuffer maps,
    /// descriptor bindings, cohorts, swapchain images) are kept — they reflect
    /// live objects and are repopulated by observation, not selection state.
    pub fn reset_graph(&self) {
        let _observations = self.surfaces.observations.transaction();
        self.surfaces.observations.clear();
    }

    /// Selects up to `limit` offscreen sources for the presented swapchain
    /// image, walking presentation roots through the dependency graph. Each
    /// returned source is one whole screen; the caller streams them separately.
    pub fn take_present_sources(
        &self,
        presented_image: vk::Image,
        limit: usize,
    ) -> Vec<PresentSource> {
        let _observations = self.surfaces.observations.transaction();
        self.select_presented(presented_image, limit)
    }
}

#[cfg(test)]
fn rect_eq(left: vk::Rect2D, right: vk::Rect2D) -> bool {
    left.offset.x == right.offset.x
        && left.offset.y == right.offset.y
        && left.extent.width == right.extent.width
        && left.extent.height == right.extent.height
}

fn viewport_rect(viewport: vk::Viewport) -> Option<vk::Rect2D> {
    if !viewport.x.is_finite()
        || !viewport.y.is_finite()
        || !viewport.width.is_finite()
        || !viewport.height.is_finite()
    {
        return None;
    }
    let other_x = viewport.x + viewport.width;
    let other_y = viewport.y + viewport.height;
    let left = f64::from(viewport.x.min(other_x)).floor() as i64;
    let top = f64::from(viewport.y.min(other_y)).floor() as i64;
    let right = f64::from(viewport.x.max(other_x)).ceil() as i64;
    let bottom = f64::from(viewport.y.max(other_y)).ceil() as i64;
    if right <= left || bottom <= top {
        return None;
    }
    Some(vk::Rect2D {
        offset: vk::Offset2D {
            x: i32::try_from(left).ok()?,
            y: i32::try_from(top).ok()?,
        },
        extent: vk::Extent2D {
            width: u32::try_from(right - left).ok()?,
            height: u32::try_from(bottom - top).ok()?,
        },
    })
}

impl Tracking {
    fn intersect_rect(left: vk::Rect2D, right: vk::Rect2D) -> Option<vk::Rect2D> {
        let rect = |value: vk::Rect2D| polymer_presentation::Rect {
            x: value.offset.x,
            y: value.offset.y,
            extent: polymer_presentation::Extent2D {
                width: value.extent.width,
                height: value.extent.height,
            },
        };
        let result = rect(left).intersect(rect(right))?;
        Some(vk::Rect2D {
            offset: vk::Offset2D {
                x: result.x,
                y: result.y,
            },
            extent: vk::Extent2D {
                width: result.extent.width,
                height: result.extent.height,
            },
        })
    }
}

fn raster_rect(
    viewport: Option<vk::Viewport>,
    scissor: Option<vk::Rect2D>,
    bounds: vk::Extent2D,
) -> Option<vk::Rect2D> {
    let viewport = viewport.and_then(viewport_rect);
    let rect = match (viewport, scissor) {
        (Some(viewport), Some(scissor)) => Tracking::intersect_rect(viewport, scissor)?,
        (Some(viewport), None) => viewport,
        (None, Some(scissor)) => scissor,
        (None, None) => return None,
    };
    Tracking::clamp_rect(rect, bounds)
}

#[cfg(test)]
mod tests {
    use super::*;
    use ash::vk::Handle;

    fn image(value: u64) -> vk::Image {
        vk::Image::from_raw(value)
    }

    fn extent(width: u32, height: u32) -> vk::Extent2D {
        vk::Extent2D { width, height }
    }

    fn rect(x: i32, y: i32, width: u32, height: u32) -> vk::Rect2D {
        vk::Rect2D {
            offset: vk::Offset2D { x, y },
            extent: extent(width, height),
        }
    }

    #[test]
    fn tracking_instances_do_not_share_images() {
        let first = Tracking::default();
        let second = Tracking::default();
        let image = image(1);
        let extent = extent(640, 480);

        first.note_image(image, extent);

        let tracked = first
            .image_extent(image)
            .expect("first instance tracks the image");
        assert_eq!((tracked.width, tracked.height), (640, 480));
        assert!(second.image_extent(image).is_none());
    }

    #[test]
    fn swapchain_image_registration_keeps_presentation_extent_out_of_graph_images() {
        let tracking = Tracking::default();
        let image = image(1);

        tracking.swapchain_image_added(image, extent(400, 480));

        let tracked = tracking
            .swapchain_extent(image)
            .expect("swapchain image extent should be tracked");
        assert_eq!((tracked.width, tracked.height), (400, 480));
        assert!(tracking.image_extent(image).is_none());
        assert!(tracking.is_swapchain_image(image));

        tracking.swapchain_image_removed(image);
        assert!(tracking.swapchain_extent(image).is_none());
        assert!(!tracking.is_swapchain_image(image));
    }

    fn present_source(image: vk::Image, width: u32, height: u32) -> PresentSource {
        PresentSource {
            logical_slot: None,
            subresource: (0, 0),
            incarnation: 0,
            selection: polymer_presentation::SurfaceSelection {
                source_rect: polymer_presentation::Extent2D { width, height }.rect(),
                ..Default::default()
            },
            allocation_extent: extent(width, height),
            image,
            layout: vk::ImageLayout::SHADER_READ_ONLY_OPTIMAL,
        }
    }

    #[test]
    fn revalidation_drops_destroyed_images_and_rejects_out_of_bounds_crops() {
        let tracking = Tracking::default();
        let (top, bottom) = (image(1), image(2));
        tracking.note_image(top, extent(400, 240));
        tracking.note_image(bottom, extent(320, 240));
        let mut sources = [
            present_source(top, 400, 240),
            present_source(bottom, 320, 240),
        ];
        for source in &mut sources {
            source.incarnation = tracking.surface_id(source.image, 0, 0).incarnation;
        }
        assert_eq!(tracking.refresh_sources(&sources).len(), 2);

        // A resolution switch destroys the old screen; its extent record goes
        // with it, and the held selection is now reallocated.
        tracking.forget_image(bottom);
        assert_eq!(tracking.refresh_sources(&sources).len(), 1);
        sources[0].selection.source_rect.x = 1;
        assert!(tracking.refresh_source(&sources[0]).is_none());
    }

    #[test]
    fn reset_graph_clears_selection_but_keeps_live_images() {
        let tracking = Tracking::default();
        let (destination, source) = (image(1), image(2));
        tracking.note_image(destination, extent(400, 240));
        tracking.note_image(source, extent(400, 240));
        let command = vk::CommandBuffer::from_raw(3);
        tracking.command_blitted(
            command,
            source,
            vk::ImageLayout::TRANSFER_SRC_OPTIMAL,
            destination,
            vk::Offset2D::default(),
            extent(400, 240),
            vk::Offset2D::default(),
            extent(400, 240),
            (false, false),
        );
        tracking.commands_submitted(&[command]);
        assert!(
            !tracking
                .surfaces
                .observations
                .graph(0)
                .observation_list()
                .1
                .is_empty()
        );
        tracking.reset_graph();
        assert!(
            tracking
                .surfaces
                .observations
                .graph(0)
                .observation_list()
                .1
                .is_empty()
        );
        // Live-object descriptors survive: the images still exist.
        assert!(tracking.image_extent(destination).is_some());
        assert!(tracking.image_extent(source).is_some());
    }

    #[test]
    fn raster_bounds_intersect_scissor_with_a_negative_height_viewport() {
        let viewport = vk::Viewport {
            x: 20.0,
            y: 600.0,
            width: 800.0,
            height: -500.0,
            min_depth: 0.0,
            max_depth: 1.0,
        };

        assert!(rect_eq(
            raster_rect(
                Some(viewport),
                Some(rect(100, 50, 640, 480)),
                extent(1280, 720),
            )
            .expect("viewport and scissor overlap"),
            rect(100, 100, 640, 430),
        ));
    }
}

// ---------------------------------------------------------------------------
// Render-target candidate heuristics and per-command-buffer state, fed by the
// hook layer's raw command observations.
// ---------------------------------------------------------------------------

#[derive(Clone, Default)]
struct CommandTrackingState {
    framebuffer: Option<vk::Framebuffer>,
    descriptor_sets: Vec<vk::DescriptorSet>,
    pushed_images: Vec<(surfaces::SurfaceId, vk::ImageLayout)>,
    viewport: Option<vk::Viewport>,
    scissor: Option<vk::Rect2D>,
    dirty: bool,
}

impl Tracking {
    pub fn swapchain_image_added(&self, image: vk::Image, extent: vk::Extent2D) {
        self.swapchain_images.insert(image, extent);
    }

    pub fn swapchain_image_removed(&self, image: vk::Image) {
        self.swapchain_images.remove(&image);
        self.forget_surfaces(image);
    }

    pub fn is_swapchain_image(&self, image: vk::Image) -> bool {
        self.swapchain_images.contains_key(&image)
    }

    fn swapchain_extent(&self, image: vk::Image) -> Option<vk::Extent2D> {
        self.swapchain_images.get(&image).map(|entry| *entry)
    }

    pub fn clamp_rect(rect: vk::Rect2D, bounds: vk::Extent2D) -> Option<vk::Rect2D> {
        let left = rect.offset.x.max(0) as u32;
        let top = rect.offset.y.max(0) as u32;
        let right = (i64::from(rect.offset.x) + i64::from(rect.extent.width))
            .clamp(0, i64::from(bounds.width)) as u32;
        let bottom = (i64::from(rect.offset.y) + i64::from(rect.extent.height))
            .clamp(0, i64::from(bounds.height)) as u32;
        (right > left && bottom > top).then_some(vk::Rect2D {
            offset: vk::Offset2D {
                x: left as i32,
                y: top as i32,
            },
            extent: vk::Extent2D {
                width: right - left,
                height: bottom - top,
            },
        })
    }

    pub fn capture_rect(&self, image: vk::Image, bounds: vk::Extent2D) -> Option<vk::Rect2D> {
        if !polymer_hook::config::tracking_enabled() {
            return None;
        }
        let observations = self.surfaces.observations.graph(self.generation());
        let content = polymer_presentation::PresentationSelector::observed_content(
            polymer_presentation::Extent2D {
                width: bounds.width,
                height: bounds.height,
            },
            observations
                .edges
                .values()
                .flatten()
                .filter(|edge| edge.destination.image == image)
                .filter_map(|edge| edge.coverage.map(|rect| (edge.generation, rect))),
        )?;
        Some(vk::Rect2D {
            offset: vk::Offset2D {
                x: content.x,
                y: content.y,
            },
            extent: vk::Extent2D {
                width: content.extent.width,
                height: content.extent.height,
            },
        })
    }

    pub fn image_created(
        &self,
        image: vk::Image,
        extent: vk::Extent2D,
        format: vk::Format,
        usage: vk::ImageUsageFlags,
    ) {
        self.note_image(image, extent);
        let usage = usage | vk::ImageUsageFlags::TRANSFER_SRC;
        self.image_info.insert(image, (format, usage));
    }

    pub fn image_destroyed(&self, image: vk::Image) {
        self.image_info.remove(&image);
        self.forget_image(image);
    }

    pub fn image_view_created(
        &self,
        view: vk::ImageView,
        image: vk::Image,
        range: vk::ImageSubresourceRange,
    ) {
        let surfaces = self.view_surfaces(image, range);
        self.image_view.insert(view, surfaces.clone());
        let color = range.aspect_mask.contains(vk::ImageAspectFlags::COLOR);
        if color {
            self.color_image_view.insert(view, surfaces);
        }
    }

    pub fn image_view_destroyed(&self, view: vk::ImageView) {
        self.image_view.remove(&view);
        self.color_image_view.remove(&view);
    }

    pub fn framebuffer_created(&self, framebuffer: vk::Framebuffer, attachments: &[vk::ImageView]) {
        let images = attachments
            .iter()
            .flat_map(|view| {
                self.color_image_view
                    .get(view)
                    .map_or_else(Vec::new, |entry| entry.clone())
            })
            .collect();
        self.framebuffer_images.insert(framebuffer, images);
    }

    pub fn framebuffer_destroyed(&self, framebuffer: vk::Framebuffer) {
        self.framebuffer_images.remove(&framebuffer);
    }

    pub fn descriptor_images_written(
        &self,
        set: vk::DescriptorSet,
        entries: &[(u32, u32, vk::ImageView, vk::ImageLayout)],
    ) {
        for &(binding, element, view, layout) in entries {
            if let Some(image) = self.image_view.get(&view).map(|entry| entry.clone()) {
                self.descriptor_set_images
                    .entry(set)
                    .or_default()
                    .insert((binding, element), (image, layout));
            }
        }
    }

    pub fn command_buffer_began(&self, command_buffer: vk::CommandBuffer) {
        let mut pending = vec![command_buffer];
        let mut seen = std::collections::HashSet::new();
        while let Some(command) = pending.pop() {
            if !seen.insert(command) {
                continue;
            }
            pending.extend(
                self.command_dependencies
                    .iter()
                    .filter(|entry| entry.contains(&command))
                    .map(|entry| *entry.key()),
            );
            self.command_dependencies.remove(&command);
            self.command_observations.remove(&command);
            self.command_tracking.remove(&command);
        }
    }

    pub fn command_bound_descriptor_sets(
        &self,
        command_buffer: vk::CommandBuffer,
        first_set: u32,
        sets: &[vk::DescriptorSet],
    ) {
        if sets.is_empty() {
            return;
        }
        let mut state = self.command_tracking.entry(command_buffer).or_default();
        let required = first_set as usize + sets.len();
        state
            .descriptor_sets
            .resize(required, vk::DescriptorSet::null());
        state.descriptor_sets[first_set as usize..required].copy_from_slice(sets);
        state.dirty = true;
    }

    pub fn command_pushed_descriptors(
        &self,
        command_buffer: vk::CommandBuffer,
        views: &[(vk::ImageView, vk::ImageLayout)],
    ) {
        let pushed_images = views
            .iter()
            .flat_map(|&(view, layout)| {
                self.image_view.get(&view).map_or_else(Vec::new, |entry| {
                    entry.iter().map(|id| (*id, layout)).collect()
                })
            })
            .collect();
        let mut state = self.command_tracking.entry(command_buffer).or_default();
        state.pushed_images = pushed_images;
        state.dirty = true;
    }

    pub fn command_set_viewport(&self, command_buffer: vk::CommandBuffer, viewport: vk::Viewport) {
        let mut state = self.command_tracking.entry(command_buffer).or_default();
        state.viewport = Some(viewport);
        state.dirty = true;
    }

    pub fn command_set_scissor(&self, command_buffer: vk::CommandBuffer, scissor: vk::Rect2D) {
        let mut state = self.command_tracking.entry(command_buffer).or_default();
        state.scissor = Some(scissor);
        state.dirty = true;
    }

    pub fn command_began_render_pass(
        &self,
        command_buffer: vk::CommandBuffer,
        framebuffer: vk::Framebuffer,
    ) {
        let mut state = self.command_tracking.entry(command_buffer).or_default();
        state.framebuffer = Some(framebuffer);
        state.dirty = true;
    }

    pub fn command_ended_render_pass(&self, command_buffer: vk::CommandBuffer) {
        if let Some(mut state) = self.command_tracking.get_mut(&command_buffer) {
            state.framebuffer = None;
            state.dirty = true;
        }
    }

    /// The draw observer. Feeds both the one-hop candidate heuristic (default)
    /// and the dependency graph (opt-in).
    pub fn command_drew(&self, command_buffer: vk::CommandBuffer) {
        let Some(mut state) = self.command_tracking.get_mut(&command_buffer) else {
            return;
        };
        if !state.dirty {
            return;
        }
        state.dirty = false;
        self.command_observations
            .entry(command_buffer)
            .or_default()
            .push(commands::RecordedCommand::SurfaceDraw(state.clone()));
    }
}
