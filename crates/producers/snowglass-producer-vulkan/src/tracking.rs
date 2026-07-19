//! Presentation dependency-graph tracking.
//!
//! Persistent, generation-stamped observations of which images are rendered
//! into, what they were filled from, and what reached the swapchain. The
//! graph is what lets a capture pick the offscreen screens a compositor
//! samples instead of the composite it blits to the swapchain. The hook
//! layer forwards raw command observations; all selection policy lives here,
//! and each selected source is expected to become its own capture stream —
//! composition is never this crate's concern.

use ash::vk;
use dashmap::DashMap;
use std::{
    collections::{HashMap, HashSet},
    sync::atomic::{AtomicU64, Ordering},
};

/// Present-count clock used to age out observations. Presentation uses
/// images produced in earlier frames (page-flipped guest framebuffers,
/// mailbox frames filled by a worker thread), so observations must outlive
/// the frame that recorded them; anything untouched for [`STALE_HORIZON`]
/// presents is evicted.
const STALE_HORIZON: u64 = 8;

struct Target {
    extent: vk::Extent2D,
    score: u64,
    seen: u64,
}

/// Device-scoped state shared by the Vulkan hook contexts.
#[derive(Default)]
pub struct Tracking {
    generation: AtomicU64,
    image_extent: DashMap<vk::Image, vk::Extent2D>,
    targets: DashMap<vk::Image, Target>,
    dependencies: DashMap<vk::Image, Vec<(vk::Image, u64)>>,
    read_layout: DashMap<vk::Image, vk::ImageLayout>,
    presented: DashMap<vk::Image, Vec<(vk::Image, bool)>>,
    render_target: DashMap<vk::Image, RenderTargetCandidate>,
    image_info: DashMap<vk::Image, (vk::Format, vk::ImageUsageFlags)>,
    image_view: DashMap<vk::ImageView, vk::Image>,
    color_image_view: DashMap<vk::ImageView, vk::Image>,
    framebuffer_images: DashMap<vk::Framebuffer, Vec<vk::Image>>,
    descriptor_set_images:
        DashMap<vk::DescriptorSet, HashMap<(u32, u32), (vk::Image, vk::ImageLayout)>>,
    image_cohort: DashMap<(i32, u32, u32, u32), u32>,
    command_tracking: DashMap<vk::CommandBuffer, CommandTrackingState>,
    command_candidate: DashMap<vk::CommandBuffer, (vk::Image, RenderTargetCandidate)>,
    swapchain_images: DashMap<vk::Image, ()>,
    present_rect: DashMap<vk::Image, (vk::Rect2D, u64)>,
}

/// One selected capture source: a whole presented screen.
#[derive(Clone, Copy)]
pub struct PresentSource {
    pub image: vk::Image,
    pub extent: vk::Extent2D,
    pub layout: vk::ImageLayout,
}

impl Tracking {
    fn generation(&self) -> u64 {
        self.generation.load(Ordering::Relaxed)
    }

    pub fn note_image(&self, image: vk::Image, extent: vk::Extent2D) {
        self.image_extent.insert(image, extent);
    }

    pub fn image_extent(&self, image: vk::Image) -> Option<vk::Extent2D> {
        self.image_extent.get(&image).map(|entry| *entry)
    }

    pub fn forget_image(&self, image: vk::Image) {
        self.image_extent.remove(&image);
        self.targets.remove(&image);
        self.dependencies.remove(&image);
        self.read_layout.remove(&image);
        self.presented.remove(&image);
    }

    pub fn record_target(&self, image: vk::Image, score: u64) {
        if !snowglass_hook::config::graph_tracking_enabled() {
            return;
        }
        let Some(extent) = self.image_extent(image) else {
            return;
        };
        self.upsert_target(image, extent, score);
    }

    fn upsert_target(&self, image: vk::Image, extent: vk::Extent2D, score: u64) {
        let now = self.generation();
        match self.targets.entry(image) {
            dashmap::mapref::entry::Entry::Occupied(mut entry) => {
                let target = entry.get_mut();
                if score >= target.score || target.seen + STALE_HORIZON < now {
                    target.score = score;
                }
                target.extent = extent;
                target.seen = now;
            }
            dashmap::mapref::entry::Entry::Vacant(entry) => {
                entry.insert(Target {
                    extent,
                    score,
                    seen: now,
                });
            }
        }
    }

    pub fn record_dependencies(
        &self,
        destination: vk::Image,
        sampled: &[(vk::Image, vk::ImageLayout)],
    ) {
        if sampled.is_empty() || !snowglass_hook::config::graph_tracking_enabled() {
            return;
        }
        let now = self.generation();
        let mut inputs = self.dependencies.entry(destination).or_default();
        for &(image, layout) in sampled {
            if image == destination {
                continue;
            }
            self.read_layout.insert(image, layout);
            if let Some((_, seen)) = inputs.iter_mut().find(|(input, _)| *input == image) {
                *seen = now;
            } else {
                inputs.push((image, now));
            }
        }
    }

    /// An offscreen blit or copy makes the destination a render target fed by
    /// the source; display-transfer emulation moves finished frames this way.
    pub fn record_transfer(
        &self,
        source: vk::Image,
        source_layout: vk::ImageLayout,
        destination: vk::Image,
    ) {
        if !snowglass_hook::config::graph_tracking_enabled() {
            return;
        }
        let Some(extent) = self.image_extent(destination) else {
            return;
        };
        self.read_layout.insert(source, source_layout);
        self.touch(&[source]);
        self.upsert_target(
            destination,
            extent,
            u64::from(extent.width) * u64::from(extent.height),
        );
        let now = self.generation();
        let mut inputs = self.dependencies.entry(destination).or_default();
        if let Some((_, seen)) = inputs.iter_mut().find(|(input, _)| *input == source) {
            *seen = now;
        } else {
            inputs.push((source, now));
        }
    }

    pub fn record_presented(
        &self,
        swapchain_image: vk::Image,
        sources: &[(vk::Image, vk::ImageLayout)],
        via_blit: bool,
    ) {
        if sources.is_empty() || !snowglass_hook::config::graph_tracking_enabled() {
            return;
        }
        {
            let mut presented = self.presented.entry(swapchain_image).or_default();
            for &(image, layout) in sources {
                self.read_layout.insert(image, layout);
                if let Some((_, blit)) = presented.iter_mut().find(|(source, _)| *source == image) {
                    *blit |= via_blit;
                } else {
                    presented.push((image, via_blit));
                }
            }
        }
        self.touch(&sources.iter().map(|(image, _)| *image).collect::<Vec<_>>());
    }

    /// Refreshes the staleness stamps of images that are still reaching the
    /// screen. A static scene re-presents buffers without redrawing them, and
    /// their history must not decay while that lasts.
    fn touch(&self, images: &[vk::Image]) {
        let now = self.generation();
        for image in images {
            if let Some(mut target) = self.targets.get_mut(image) {
                target.seen = now;
            }
        }
    }

    /// Re-resolves a previous selection against the live stores, dropping
    /// images that have been destroyed or aged out so a stale selection cannot
    /// outlive the buffers it names. Entries stay live only while the
    /// application keeps rendering into them; this deliberately does not refresh
    /// their stamps.
    pub fn refresh_sources(&self, images: &[vk::Image]) -> Vec<PresentSource> {
        images
            .iter()
            .filter_map(|&image| {
                let target = self.targets.get(&image)?;
                Some(PresentSource {
                    image,
                    extent: target.extent,
                    layout: self
                        .read_layout
                        .get(&image)
                        .map_or(vk::ImageLayout::GENERAL, |layout| *layout),
                })
            })
            .collect()
    }

    /// Selects up to `limit` offscreen sources for the presented swapchain
    /// image, walking presentation roots through the dependency graph. Each
    /// returned source is one whole screen; the caller streams them separately.
    pub fn take_present_sources(
        &self,
        presented_image: vk::Image,
        limit: usize,
    ) -> Vec<PresentSource> {
        let now = self.generation.fetch_add(1, Ordering::Relaxed) + 1;
        let horizon = now.saturating_sub(STALE_HORIZON);
        self.targets.retain(|_, target| target.seen >= horizon);
        self.dependencies.retain(|_, inputs| {
            inputs.retain(|(_, seen)| *seen >= horizon);
            !inputs.is_empty()
        });
        let Some((_, presented)) = self.presented.remove(&presented_image) else {
            return Vec::new();
        };
        // A wholesale blit to the swapchain is definitive: its source is the
        // frame even when nothing rendered into it through observed commands,
        // so give such roots a stand-in target if rendering never made one.
        for &(image, via_blit) in &presented {
            if via_blit
                && !self.targets.contains_key(&image)
                && let Some(extent) = self.image_extent(image)
            {
                self.upsert_target(
                    image,
                    extent,
                    u64::from(extent.width) * u64::from(extent.height),
                );
            }
        }
        let dependencies = self
            .dependencies
            .iter()
            .map(|entry| (*entry.key(), entry.value().clone()))
            .collect::<HashMap<_, _>>();
        let mut blit_roots = presented
            .iter()
            .filter(|(_, via_blit)| *via_blit)
            .map(|(image, _)| *image)
            .collect::<Vec<_>>();
        self.collapse_flip_pairs(&mut blit_roots, &dependencies);
        let all_roots = presented
            .iter()
            .map(|(image, _)| *image)
            .collect::<Vec<_>>();
        // Trust blit roots exclusively when they resolve: they are exact, and
        // independently blitted screens may differ in extent. Draw-sampled
        // roots keep the extent cohort, which filters UI overlay textures.
        let selected = self.select(&blit_roots, &dependencies, limit, true);
        let selected = if selected.is_empty() {
            self.largest_extent_cohort(self.select(&all_roots, &dependencies, limit, false))
        } else {
            selected
        };
        selected
            .into_iter()
            .filter_map(|image| {
                let target = self.targets.get(&image)?;
                Some(PresentSource {
                    image,
                    extent: target.extent,
                    layout: self
                        .read_layout
                        .get(&image)
                        .map_or(vk::ImageLayout::GENERAL, |layout| *layout),
                })
            })
            .collect()
    }

    fn select(
        &self,
        roots: &[vk::Image],
        dependencies: &HashMap<vk::Image, Vec<(vk::Image, u64)>>,
        limit: usize,
        decompose_roots: bool,
    ) -> Vec<vk::Image> {
        let area = |image: &vk::Image| {
            self.targets.get(image).map_or(0, |target| {
                u64::from(target.extent.width) * u64::from(target.extent.height)
            })
        };
        let mut frontier = roots
            .iter()
            .copied()
            .filter(|image| self.targets.contains_key(image))
            .collect::<Vec<_>>();
        Self::deduplicate(&mut frontier);
        if frontier.is_empty() {
            return Vec::new();
        }
        // If both an intermediate and one of its inputs reached the screen,
        // retain the downstream node; expanding it recovers all independent
        // inputs without double-counting the branch.
        let roots_snapshot = frontier.clone();
        frontier.retain(|image| {
            !roots_snapshot
                .iter()
                .any(|other| other != image && self.reachable(*other, *image, dependencies))
        });
        // Blit-presented roots get one round of decomposition even when the
        // limit is already met: a wholesale-blitted composite should yield
        // its biggest screen rather than the whole window layout. Deeper
        // nodes (and draw-sampled roots, whose sampled sets include HUD and
        // portrait textures) only expand while room remains.
        let root_set = if decompose_roots {
            frontier.iter().copied().collect::<HashSet<_>>()
        } else {
            HashSet::new()
        };
        loop {
            let Some((index, inputs)) = frontier.iter().enumerate().find_map(|(index, image)| {
                if frontier.len() >= limit && !root_set.contains(image) {
                    return None;
                }
                let mut inputs = dependencies
                    .get(image)?
                    .iter()
                    .map(|(input, _)| *input)
                    .filter(|input| input != image && self.targets.contains_key(input))
                    .collect::<Vec<_>>();
                if inputs.is_empty() {
                    return None;
                }
                // A composite redrawn every few presents accumulates both
                // faces of each page-flipped screen it samples; keep only the
                // freshest face of each before deciding to decompose.
                self.collapse_flip_pairs(&mut inputs, dependencies);
                // Expanding decomposes a composite into the screens drawn
                // into it. A single input of equal or larger extent is a copy
                // or scaling chain, not a composition; descending it would
                // trade the presented frame for the pipeline producing it.
                let distinct = inputs.iter().copied().collect::<HashSet<_>>();
                if distinct.len() < 2 && inputs.iter().all(|input| area(input) >= area(image)) {
                    return None;
                }
                Some((index, inputs))
            }) else {
                break;
            };
            frontier.remove(index);
            frontier.splice(index..index, inputs);
            Self::deduplicate(&mut frontier);
        }
        // The recorded order varies with which mailbox frame happened to be
        // presented, and a capture that reorders its screens every frame
        // reads as flicker. Order deterministically instead: tallest and
        // widest first (a 3DS top screen before the bottom one), with the
        // stable sort preserving arrival order between equals.
        frontier.sort_by_key(|image| {
            self.targets.get(image).map_or((0, 0), |target| {
                (
                    u32::MAX - target.extent.height,
                    u32::MAX - target.extent.width,
                )
            })
        });
        frontier.truncate(limit);
        frontier
    }

    fn reachable(
        &self,
        start: vk::Image,
        needle: vk::Image,
        dependencies: &HashMap<vk::Image, Vec<(vk::Image, u64)>>,
    ) -> bool {
        let mut pending = vec![start];
        let mut visited = HashSet::new();
        while let Some(image) = pending.pop() {
            if !visited.insert(image) {
                continue;
            }
            let Some(inputs) = dependencies.get(&image) else {
                continue;
            };
            if inputs.iter().any(|(input, _)| *input == needle) {
                return true;
            }
            pending.extend(
                inputs
                    .iter()
                    .filter(|(input, _)| self.targets.contains_key(input))
                    .map(|(input, _)| *input),
            );
        }
        false
    }

    /// Page-flipped guest framebuffers alternate two images for one screen,
    /// and both faces can reach the screen within one capture interval. Two
    /// roots with the same extent filled from a shared source are one logical
    /// screen; the one whose shared edge is freshest survives.
    fn collapse_flip_pairs(
        &self,
        roots: &mut Vec<vk::Image>,
        dependencies: &HashMap<vk::Image, Vec<(vk::Image, u64)>>,
    ) {
        let extent_of = |image: &vk::Image| {
            self.targets
                .get(image)
                .map(|target| (target.extent.width, target.extent.height))
        };
        let inputs_of = |image: &vk::Image| {
            dependencies
                .get(image)
                .map(|inputs| inputs.iter().copied().collect::<HashMap<_, _>>())
                .unwrap_or_default()
        };
        let mut removed = HashSet::new();
        let snapshot = roots.clone();
        for (index, &first) in snapshot.iter().enumerate() {
            for &second in &snapshot[index + 1..] {
                if removed.contains(&first) || removed.contains(&second) {
                    continue;
                }
                let extent = extent_of(&first);
                if extent.is_none() || extent != extent_of(&second) {
                    continue;
                }
                let first_inputs = inputs_of(&first);
                let second_inputs = inputs_of(&second);
                let shared = first_inputs
                    .iter()
                    .filter_map(|(source, first_seen)| {
                        second_inputs
                            .get(source)
                            .map(|second_seen| (*first_seen, *second_seen))
                    })
                    .collect::<Vec<_>>();
                if shared.is_empty() {
                    continue;
                }
                let first_newest = shared.iter().map(|(seen, _)| *seen).max().unwrap_or(0);
                let second_newest = shared.iter().map(|(_, seen)| *seen).max().unwrap_or(0);
                removed.insert(if first_newest >= second_newest {
                    second
                } else {
                    first
                });
            }
        }
        roots.retain(|root| !removed.contains(root));
    }

    /// Keeps the largest group of selections sharing one extent. Applied to
    /// draw-sampled selections only, where UI composition quads can slip in
    /// alongside genuine screens.
    fn largest_extent_cohort(&self, selected: Vec<vk::Image>) -> Vec<vk::Image> {
        let extent_of = |image: &vk::Image| {
            self.targets
                .get(image)
                .map(|target| (target.extent.width, target.extent.height))
        };
        let Some(best) = selected.iter().filter_map(extent_of).max_by_key(|extent| {
            let count = selected
                .iter()
                .filter(|image| extent_of(image) == Some(*extent))
                .count();
            (count, u64::from(extent.0) * u64::from(extent.1))
        }) else {
            return selected;
        };
        selected
            .into_iter()
            .filter(|image| extent_of(image) == Some(best))
            .collect()
    }

    fn deduplicate(values: &mut Vec<vk::Image>) {
        let mut seen = HashSet::new();
        values.retain(|value| seen.insert(*value));
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use ash::vk::Handle;

    #[test]
    fn tracking_instances_do_not_share_images() {
        let first = Tracking::default();
        let second = Tracking::default();
        let image = vk::Image::from_raw(1);
        let extent = vk::Extent2D {
            width: 640,
            height: 480,
        };

        first.note_image(image, extent);

        let tracked = first
            .image_extent(image)
            .expect("first instance tracks the image");
        assert_eq!((tracked.width, tracked.height), (640, 480));
        assert!(second.image_extent(image).is_none());
    }
}

// ---------------------------------------------------------------------------
// Render-target candidate heuristics and per-command-buffer state, fed by the
// hook layer's raw command observations.
// ---------------------------------------------------------------------------

#[derive(Clone, Copy)]
pub struct RenderTargetCandidate {
    pub image: vk::Image,
    pub extent: vk::Extent2D,
    pub layout: vk::ImageLayout,
    score: u64,
    cohort_size: u32,
}

impl RenderTargetCandidate {
    pub fn is_better_than(self, other: Self) -> bool {
        (self.score, self.cohort_size) > (other.score, other.cohort_size)
    }
}

#[derive(Clone, Default)]
struct CommandTrackingState {
    framebuffer: Option<vk::Framebuffer>,
    descriptor_sets: Vec<vk::DescriptorSet>,
    pushed_images: Vec<(vk::Image, vk::ImageLayout)>,
    viewport: Option<vk::Viewport>,
    scissor: Option<vk::Rect2D>,
    dirty: bool,
}

impl Tracking {
    pub fn command_candidate(
        &self,
        command_buffer: vk::CommandBuffer,
    ) -> Option<(vk::Image, RenderTargetCandidate)> {
        self.command_candidate
            .get(&command_buffer)
            .map(|entry| *entry)
    }

    pub fn render_target(&self, image: vk::Image) -> Option<RenderTargetCandidate> {
        self.render_target.get(&image).map(|entry| *entry)
    }

    pub fn remove_render_target(&self, image: vk::Image) {
        self.render_target.remove(&image);
    }

    pub fn swapchain_image_added(&self, image: vk::Image) {
        self.swapchain_images.insert(image, ());
    }

    pub fn swapchain_image_removed(&self, image: vk::Image) {
        self.swapchain_images.remove(&image);
        self.render_target.remove(&image);
        self.present_rect.remove(&image);
    }

    pub fn is_swapchain_image(&self, image: vk::Image) -> bool {
        self.swapchain_images.contains_key(&image)
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

    fn record_present_rect(&self, image: vk::Image, rect: vk::Rect2D, draw_area: u64) {
        let replace = self
            .present_rect
            .get(&image)
            .is_none_or(|current| draw_area >= current.1);
        if replace {
            self.present_rect.insert(image, (rect, draw_area));
        }
    }

    pub fn capture_rect(&self, image: vk::Image, bounds: vk::Extent2D) -> Option<vk::Rect2D> {
        snowglass_hook::config::tracking_enabled()
            .then(|| self.present_rect.get(&image).map(|entry| entry.0))
            .flatten()
            .and_then(|rect| Self::clamp_rect(rect, bounds))
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
        self.image_cohort
            .entry((format.as_raw(), usage.as_raw(), extent.width, extent.height))
            .and_modify(|count| *count += 1)
            .or_insert(1);
    }

    pub fn image_destroyed(&self, image: vk::Image) {
        let cohort_key = self.image_info.get(&image).and_then(|info| {
            self.image_extent(image).map(|extent| {
                (
                    info.0.as_raw(),
                    info.1.as_raw(),
                    extent.width,
                    extent.height,
                )
            })
        });
        self.image_info.remove(&image);
        self.present_rect.remove(&image);
        self.render_target.remove(&image);
        self.forget_image(image);
        if let Some(key) = cohort_key
            && let Some(mut count) = self.image_cohort.get_mut(&key)
        {
            *count = count.saturating_sub(1);
            let remove = *count == 0;
            drop(count);
            if remove {
                self.image_cohort.remove(&key);
            }
        }
    }

    pub fn image_view_created(&self, view: vk::ImageView, image: vk::Image, color: bool) {
        self.image_view.insert(view, image);
        if color {
            self.color_image_view.insert(view, image);
        }
    }

    pub fn image_view_destroyed(&self, view: vk::ImageView) {
        self.image_view.remove(&view);
        self.color_image_view.remove(&view);
    }

    pub fn framebuffer_created(&self, framebuffer: vk::Framebuffer, attachments: &[vk::ImageView]) {
        let images = attachments
            .iter()
            .filter_map(|view| self.color_image_view.get(view).map(|entry| *entry))
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
            if let Some(image) = self.image_view.get(&view).map(|entry| *entry) {
                self.descriptor_set_images
                    .entry(set)
                    .or_default()
                    .insert((binding, element), (image, layout));
            }
        }
    }

    pub fn command_buffer_began(&self, command_buffer: vk::CommandBuffer) {
        self.command_tracking.remove(&command_buffer);
        self.command_candidate.remove(&command_buffer);
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
            .filter_map(|&(view, layout)| self.image_view.get(&view).map(|entry| (*entry, layout)))
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

    fn image_cohort_size(&self, image: vk::Image, extent: vk::Extent2D) -> u32 {
        let Some(info) = self.image_info.get(&image).map(|entry| *entry) else {
            return 0;
        };
        self.image_cohort
            .get(&(
                info.0.as_raw(),
                info.1.as_raw(),
                extent.width,
                extent.height,
            ))
            .map_or(0, |entry| *entry)
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
        let Some(framebuffer) = state.framebuffer else {
            return;
        };
        let Some(images) = self.framebuffer_images.get(&framebuffer) else {
            return;
        };
        let Some(destination) = images.first().copied() else {
            return;
        };
        drop(images);
        let draw_area = state
            .scissor
            .map(|rect| u64::from(rect.extent.width) * u64::from(rect.extent.height))
            .or_else(|| {
                state
                    .viewport
                    .map(|vp| (vp.width.abs() as u64) * (vp.height.abs() as u64))
            })
            .unwrap_or(0);
        if draw_area == 0 {
            return;
        }
        let mut best = None;
        let mut consider = |image: vk::Image, layout: vk::ImageLayout| {
            if image == destination {
                return;
            }
            let Some(extent) = self.image_extent(image) else {
                return;
            };
            let pixels = u64::from(extent.width) * u64::from(extent.height);
            if pixels < 64 * 64 {
                return;
            }
            let cohort = self.image_cohort_size(image, extent);
            if best.is_none_or(|candidate: RenderTargetCandidate| {
                cohort > candidate.cohort_size
                    || (cohort == candidate.cohort_size
                        && pixels
                            > u64::from(candidate.extent.width)
                                * u64::from(candidate.extent.height))
            }) {
                best = Some(RenderTargetCandidate {
                    image,
                    extent,
                    layout,
                    score: draw_area,
                    cohort_size: cohort,
                });
            }
        };
        // The dependency graph tracks every plausibly sampled offscreen image,
        // not just the strongest: a compositor pass samples each screen once and
        // all of them matter when the presented composite is decomposed.
        let mut sampled = Vec::<(vk::Image, vk::ImageLayout)>::new();
        for set in state
            .descriptor_sets
            .iter()
            .copied()
            .filter(|set| *set != vk::DescriptorSet::null())
        {
            let Some(images) = self.descriptor_set_images.get(&set) else {
                continue;
            };
            for &(image, layout) in images.values() {
                consider(image, layout);
                if image != destination && !sampled.iter().any(|(seen, _)| *seen == image) {
                    sampled.push((image, layout));
                }
            }
        }
        for &(image, layout) in &state.pushed_images {
            consider(image, layout);
            if image != destination && !sampled.iter().any(|(seen, _)| *seen == image) {
                sampled.push((image, layout));
            }
        }
        sampled.retain(|(image, _)| {
            self.image_extent(*image)
                .is_some_and(|extent| u64::from(extent.width) * u64::from(extent.height) >= 64 * 64)
        });
        if self.is_swapchain_image(destination) {
            self.record_presented(destination, &sampled, false);
        } else {
            self.record_target(destination, draw_area);
            self.record_dependencies(destination, &sampled);
        }
        if let Some(candidate) = best {
            let rect = state.scissor.or_else(|| {
                state.viewport.map(|viewport| vk::Rect2D {
                    offset: vk::Offset2D {
                        x: viewport.x.max(0.0).round() as i32,
                        y: viewport.y.max(0.0).round() as i32,
                    },
                    extent: vk::Extent2D {
                        width: viewport.width.abs().round() as u32,
                        height: viewport.height.abs().round() as u32,
                    },
                })
            });
            if let Some(rect) = rect {
                self.record_present_rect(destination, rect, draw_area);
            }
            let replace = self
                .render_target
                .get(&destination)
                .is_none_or(|old| candidate.is_better_than(*old));
            if replace {
                self.render_target.insert(destination, candidate);
            }
            let replace_command = self
                .command_candidate
                .get(&command_buffer)
                .is_none_or(|old| candidate.is_better_than(old.1));
            if replace_command {
                self.command_candidate
                    .insert(command_buffer, (destination, candidate));
            }
        }
    }

    fn propagate_present_rect(
        &self,
        source_image: vk::Image,
        destination_image: vk::Image,
        source_offset: vk::Offset2D,
        source_extent: vk::Extent2D,
        destination_offset: vk::Offset2D,
        destination_extent: vk::Extent2D,
    ) {
        if source_extent.width == 0 || source_extent.height == 0 {
            return;
        }
        let Some(source_bounds) = self.image_extent(source_image) else {
            return;
        };
        let Some(rect) = self.capture_rect(source_image, source_bounds) else {
            return;
        };
        let relative_x = rect.offset.x.saturating_sub(source_offset.x).max(0) as u64;
        let relative_y = rect.offset.y.saturating_sub(source_offset.y).max(0) as u64;
        let mapped = vk::Rect2D {
            offset: vk::Offset2D {
                x: destination_offset.x
                    + (relative_x * u64::from(destination_extent.width)
                        / u64::from(source_extent.width)) as i32,
                y: destination_offset.y
                    + (relative_y * u64::from(destination_extent.height)
                        / u64::from(source_extent.height)) as i32,
            },
            extent: vk::Extent2D {
                width: (u64::from(rect.extent.width) * u64::from(destination_extent.width)
                    / u64::from(source_extent.width)) as u32,
                height: (u64::from(rect.extent.height) * u64::from(destination_extent.height)
                    / u64::from(source_extent.height)) as u32,
            },
        };
        let area = u64::from(mapped.extent.width) * u64::from(mapped.extent.height);
        if area != 0 {
            self.record_present_rect(destination_image, mapped, area);
        }
    }

    /// Blit observer: an offscreen destination joins the dependency graph, a
    /// swapchain destination is a trusted presentation.
    pub fn command_blitted(
        &self,
        source_image: vk::Image,
        source_layout: vk::ImageLayout,
        destination_image: vk::Image,
        source_offset: vk::Offset2D,
        source_extent: vk::Extent2D,
        destination_offset: vk::Offset2D,
        destination_extent: vk::Extent2D,
    ) {
        if source_image == destination_image {
            return;
        }
        if !self.is_swapchain_image(destination_image) {
            self.record_transfer(source_image, source_layout, destination_image);
            return;
        }
        self.record_presented(destination_image, &[(source_image, source_layout)], true);
        if source_extent.width != 0 && source_extent.height != 0 {
            self.propagate_present_rect(
                source_image,
                destination_image,
                source_offset,
                source_extent,
                destination_offset,
                destination_extent,
            );
            self.render_target.insert(
                destination_image,
                RenderTargetCandidate {
                    image: source_image,
                    extent: source_extent,
                    layout: source_layout,
                    score: u64::MAX,
                    cohort_size: u32::MAX,
                },
            );
        }
    }
}
