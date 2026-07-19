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

#[derive(Clone, Copy)]
struct EdgePlacement {
    source_rect: vk::Rect2D,
    destination_rect: vk::Rect2D,
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
    /// Raster placement observations keyed by `(destination, source)`. A source
    /// can be drawn more than once, so observations from the newest generation
    /// are retained separately and ambiguity is rejected during resolution.
    placements: DashMap<(vk::Image, vk::Image), Vec<EdgePlacement>>,
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
    /// Presentation images and their bounds. Keep these separate from
    /// `image_extent`: inserting a swapchain image into the graph's general
    /// image table makes presentation compositor draws participate in source
    /// selection and destabilizes the offscreen targets selected for capture.
    swapchain_images: DashMap<vk::Image, vk::Extent2D>,
    present_rect: DashMap<vk::Image, (vk::Rect2D, u64)>,
}

/// One selected capture source: a whole presented screen.
#[derive(Clone, Copy)]
pub struct PresentSource {
    pub image: vk::Image,
    pub extent: vk::Extent2D,
    pub layout: vk::ImageLayout,
    pub presentation_rect: Option<vk::Rect2D>,
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
        self.placements
            .retain(|(destination, source), _| *destination != image && *source != image);
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
        destination_rect: Option<vk::Rect2D>,
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
            if let (Some(source_extent), Some(destination_rect)) =
                (self.image_extent(image), destination_rect)
            {
                self.record_edge_placement(
                    destination,
                    image,
                    full_rect(source_extent),
                    destination_rect,
                );
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
        source_rect: vk::Rect2D,
        destination_rect: vk::Rect2D,
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
        self.record_edge_placement(destination, source, source_rect, destination_rect);
    }

    pub fn record_presented(
        &self,
        swapchain_image: vk::Image,
        sources: &[(vk::Image, vk::ImageLayout)],
        via_blit: bool,
        destination_rect: Option<vk::Rect2D>,
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
                if let (Some(source_extent), Some(destination_rect)) =
                    (self.image_extent(image), destination_rect)
                {
                    self.record_edge_placement(
                        swapchain_image,
                        image,
                        full_rect(source_extent),
                        destination_rect,
                    );
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
    pub fn refresh_sources(&self, sources: &[PresentSource]) -> Vec<PresentSource> {
        sources
            .iter()
            .filter_map(|source| {
                let target = self.targets.get(&source.image)?;
                Some(PresentSource {
                    image: source.image,
                    extent: target.extent,
                    layout: self
                        .read_layout
                        .get(&source.image)
                        .map_or(vk::ImageLayout::GENERAL, |layout| *layout),
                    presentation_rect: source.presentation_rect,
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
        self.placements.retain(|_, placements| {
            placements.retain(|placement| placement.seen >= horizon);
            !placements.is_empty()
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
                    presentation_rect: self.resolve_presentation_rect(
                        presented_image,
                        image,
                        &presented,
                        &dependencies,
                    ),
                })
            })
            .collect()
    }

    fn record_edge_placement(
        &self,
        destination: vk::Image,
        source: vk::Image,
        source_rect: vk::Rect2D,
        destination_rect: vk::Rect2D,
    ) {
        if source_rect.extent.width == 0
            || source_rect.extent.height == 0
            || destination_rect.extent.width == 0
            || destination_rect.extent.height == 0
        {
            return;
        }
        let seen = self.generation();
        let mut placements = self.placements.entry((destination, source)).or_default();
        if let Some(existing) = placements.iter_mut().find(|placement| {
            rect_eq(placement.source_rect, source_rect)
                && rect_eq(placement.destination_rect, destination_rect)
        }) {
            existing.seen = seen;
        } else {
            placements.push(EdgePlacement {
                source_rect,
                destination_rect,
                seen,
            });
        }
    }

    fn newest_edge_placement(
        &self,
        destination: vk::Image,
        source: vk::Image,
    ) -> Option<EdgePlacement> {
        let placements = self.placements.get(&(destination, source))?;
        let newest = placements.iter().map(|placement| placement.seen).max()?;
        let mut matching = placements
            .iter()
            .copied()
            .filter(|placement| placement.seen == newest);
        let placement = matching.next()?;
        matching
            .all(|other| {
                rect_eq(other.source_rect, placement.source_rect)
                    && rect_eq(other.destination_rect, placement.destination_rect)
            })
            .then_some(placement)
    }

    fn resolve_presentation_rect(
        &self,
        presented_image: vk::Image,
        selected_image: vk::Image,
        presented: &[(vk::Image, bool)],
        dependencies: &HashMap<vk::Image, Vec<(vk::Image, u64)>>,
    ) -> Option<vk::Rect2D> {
        let mut paths = Vec::new();
        let mut path = Vec::new();
        let mut visited = HashSet::new();
        find_paths(
            presented_image,
            selected_image,
            presented_image,
            presented,
            dependencies,
            &mut visited,
            &mut path,
            &mut paths,
        );
        let selected_extent = self.image_extent(selected_image)?;
        let presented_extent = self.swapchain_extent(presented_image)?;
        let resolved = paths
            .into_iter()
            .map(|path| {
                let mut rect = full_rect(selected_extent);
                for &(destination, source) in path.iter().rev() {
                    let placement = self.newest_edge_placement(destination, source)?;
                    rect = map_rect(rect, placement.source_rect, placement.destination_rect)?;
                }
                Self::clamp_rect(rect, presented_extent)
            })
            .collect::<Option<Vec<_>>>()?;
        let mut resolved = resolved.into_iter();
        let first = resolved.next()?;
        resolved.all(|rect| rect_eq(rect, first)).then_some(first)
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

fn full_rect(extent: vk::Extent2D) -> vk::Rect2D {
    vk::Rect2D {
        offset: vk::Offset2D { x: 0, y: 0 },
        extent,
    }
}

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

fn intersect_rect(left: vk::Rect2D, right: vk::Rect2D) -> Option<vk::Rect2D> {
    let x = left.offset.x.max(right.offset.x);
    let y = left.offset.y.max(right.offset.y);
    let right_edge = (i64::from(left.offset.x) + i64::from(left.extent.width))
        .min(i64::from(right.offset.x) + i64::from(right.extent.width));
    let bottom_edge = (i64::from(left.offset.y) + i64::from(left.extent.height))
        .min(i64::from(right.offset.y) + i64::from(right.extent.height));
    let width = right_edge - i64::from(x);
    let height = bottom_edge - i64::from(y);
    (width > 0 && height > 0).then_some(vk::Rect2D {
        offset: vk::Offset2D { x, y },
        extent: vk::Extent2D {
            width: width as u32,
            height: height as u32,
        },
    })
}

fn raster_rect(
    viewport: Option<vk::Viewport>,
    scissor: Option<vk::Rect2D>,
    bounds: vk::Extent2D,
) -> Option<vk::Rect2D> {
    let viewport = viewport.and_then(viewport_rect);
    let rect = match (viewport, scissor) {
        (Some(viewport), Some(scissor)) => intersect_rect(viewport, scissor)?,
        (Some(viewport), None) => viewport,
        (None, Some(scissor)) => scissor,
        (None, None) => return None,
    };
    Tracking::clamp_rect(rect, bounds)
}

fn map_rect(rect: vk::Rect2D, source: vk::Rect2D, destination: vk::Rect2D) -> Option<vk::Rect2D> {
    if source.extent.width == 0 || source.extent.height == 0 {
        return None;
    }
    let left = i64::from(rect.offset.x);
    let top = i64::from(rect.offset.y);
    let right = left + i64::from(rect.extent.width);
    let bottom = top + i64::from(rect.extent.height);
    let source_left = i64::from(source.offset.x);
    let source_top = i64::from(source.offset.y);
    let source_right = source_left + i64::from(source.extent.width);
    let source_bottom = source_top + i64::from(source.extent.height);
    if left < source_left || top < source_top || right > source_right || bottom > source_bottom {
        return None;
    }
    let map_axis =
        |value: i64, source_start: i64, source_size: u32, target_start: i32, target_size: u32| {
            f64::from(target_start)
                + (value - source_start) as f64 * f64::from(target_size) / f64::from(source_size)
        };
    let mapped_left = map_axis(
        left,
        source_left,
        source.extent.width,
        destination.offset.x,
        destination.extent.width,
    );
    let mapped_top = map_axis(
        top,
        source_top,
        source.extent.height,
        destination.offset.y,
        destination.extent.height,
    );
    let mapped_right = map_axis(
        right,
        source_left,
        source.extent.width,
        destination.offset.x,
        destination.extent.width,
    );
    let mapped_bottom = map_axis(
        bottom,
        source_top,
        source.extent.height,
        destination.offset.y,
        destination.extent.height,
    );
    let mapped_left = mapped_left.round() as i64;
    let mapped_top = mapped_top.round() as i64;
    let mapped_right = mapped_right.round() as i64;
    let mapped_bottom = mapped_bottom.round() as i64;
    if mapped_right <= mapped_left || mapped_bottom <= mapped_top {
        return None;
    }
    Some(vk::Rect2D {
        offset: vk::Offset2D {
            x: i32::try_from(mapped_left).ok()?,
            y: i32::try_from(mapped_top).ok()?,
        },
        extent: vk::Extent2D {
            width: u32::try_from(mapped_right - mapped_left).ok()?,
            height: u32::try_from(mapped_bottom - mapped_top).ok()?,
        },
    })
}

#[allow(clippy::too_many_arguments)]
fn find_paths(
    current: vk::Image,
    selected: vk::Image,
    presented_image: vk::Image,
    presented: &[(vk::Image, bool)],
    dependencies: &HashMap<vk::Image, Vec<(vk::Image, u64)>>,
    visited: &mut HashSet<vk::Image>,
    path: &mut Vec<(vk::Image, vk::Image)>,
    paths: &mut Vec<Vec<(vk::Image, vk::Image)>>,
) {
    if !visited.insert(current) {
        return;
    }
    let sources = if current == presented_image {
        presented
            .iter()
            .map(|(image, _)| *image)
            .collect::<Vec<_>>()
    } else {
        dependencies
            .get(&current)
            .map(|inputs| inputs.iter().map(|(image, _)| *image).collect())
            .unwrap_or_default()
    };
    for source in sources {
        path.push((current, source));
        if source == selected {
            paths.push(path.clone());
        } else {
            find_paths(
                source,
                selected,
                presented_image,
                presented,
                dependencies,
                visited,
                path,
                paths,
            );
        }
        path.pop();
    }
    visited.remove(&current);
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

    #[test]
    fn direct_draw_resolves_its_offset_presentation_rectangle() {
        let tracking = Tracking::default();
        let source = image(1);
        let presented_image = image(2);
        tracking.note_image(source, extent(240, 400));
        tracking.swapchain_image_added(presented_image, extent(1280, 720));
        tracking.record_edge_placement(
            presented_image,
            source,
            rect(0, 0, 240, 400),
            rect(80, 120, 960, 576),
        );

        assert!(rect_eq(
            tracking
                .resolve_presentation_rect(
                    presented_image,
                    source,
                    &[(source, false)],
                    &HashMap::new(),
                )
                .expect("direct placement should resolve"),
            rect(80, 120, 960, 576),
        ));
    }

    #[test]
    fn selected_source_carries_the_resolved_presentation_rectangle() {
        let tracking = Tracking::default();
        let source = image(1);
        let presented_image = image(2);
        let source_extent = extent(400, 240);
        tracking.note_image(source, source_extent);
        tracking.swapchain_image_added(presented_image, extent(1280, 720));
        tracking.upsert_target(source, source_extent, u64::MAX);
        tracking
            .presented
            .insert(presented_image, vec![(source, true)]);
        tracking.record_edge_placement(
            presented_image,
            source,
            full_rect(source_extent),
            rect(50, 75, 800, 480),
        );

        let selected = tracking.take_present_sources(presented_image, 1);

        assert_eq!(selected.len(), 1);
        assert!(rect_eq(
            selected[0]
                .presentation_rect
                .expect("selected source should carry its placement"),
            rect(50, 75, 800, 480),
        ));
    }

    #[test]
    fn placement_composes_scale_and_offset_through_an_intermediate() {
        let tracking = Tracking::default();
        let source = image(1);
        let composite = image(2);
        let presented_image = image(3);
        tracking.note_image(source, extent(400, 240));
        tracking.note_image(composite, extent(1280, 720));
        tracking.swapchain_image_added(presented_image, extent(1920, 1080));
        tracking.record_edge_placement(
            composite,
            source,
            rect(0, 0, 400, 240),
            rect(200, 100, 400, 240),
        );
        tracking.record_edge_placement(
            presented_image,
            composite,
            rect(0, 0, 1280, 720),
            rect(100, 50, 640, 360),
        );
        let dependencies = HashMap::from([(composite, vec![(source, 0)])]);

        assert!(rect_eq(
            tracking
                .resolve_presentation_rect(
                    presented_image,
                    source,
                    &[(composite, true)],
                    &dependencies,
                )
                .expect("intermediate placement should resolve"),
            rect(200, 100, 200, 120),
        ));
    }

    #[test]
    fn repeated_source_with_different_offsets_is_ambiguous() {
        let tracking = Tracking::default();
        let source = image(1);
        let presented_image = image(2);
        tracking.note_image(source, extent(400, 240));
        tracking.swapchain_image_added(presented_image, extent(1280, 720));
        tracking.record_edge_placement(
            presented_image,
            source,
            rect(0, 0, 400, 240),
            rect(20, 20, 400, 240),
        );
        tracking.record_edge_placement(
            presented_image,
            source,
            rect(0, 0, 400, 240),
            rect(700, 400, 400, 240),
        );

        assert!(
            tracking
                .resolve_presentation_rect(
                    presented_image,
                    source,
                    &[(source, false)],
                    &HashMap::new(),
                )
                .is_none()
        );
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

    pub fn swapchain_image_added(&self, image: vk::Image, extent: vk::Extent2D) {
        self.swapchain_images.insert(image, extent);
    }

    pub fn swapchain_image_removed(&self, image: vk::Image) {
        self.swapchain_images.remove(&image);
        self.render_target.remove(&image);
        self.present_rect.remove(&image);
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
        let Some(destination_extent) = self.image_extent(destination) else {
            return;
        };
        let Some(raster_rect) = raster_rect(state.viewport, state.scissor, destination_extent)
        else {
            return;
        };
        let draw_area = u64::from(raster_rect.extent.width) * u64::from(raster_rect.extent.height);
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
            self.record_presented(destination, &sampled, false, Some(raster_rect));
        } else {
            self.record_target(destination, draw_area);
            self.record_dependencies(destination, &sampled, Some(raster_rect));
        }
        if let Some(candidate) = best {
            self.record_present_rect(destination, raster_rect, draw_area);
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
        let source_rect = vk::Rect2D {
            offset: source_offset,
            extent: source_extent,
        };
        let destination_rect = vk::Rect2D {
            offset: destination_offset,
            extent: destination_extent,
        };
        if !self.is_swapchain_image(destination_image) {
            self.record_transfer(
                source_image,
                source_layout,
                destination_image,
                source_rect,
                destination_rect,
            );
            return;
        }
        self.record_presented(
            destination_image,
            &[(source_image, source_layout)],
            true,
            None,
        );
        self.record_edge_placement(
            destination_image,
            source_image,
            source_rect,
            destination_rect,
        );
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
