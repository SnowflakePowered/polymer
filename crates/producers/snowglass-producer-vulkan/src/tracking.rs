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
    sync::{
        LazyLock,
        atomic::{AtomicU64, Ordering},
    },
};

/// Present-count clock used to age out observations. Presentation uses
/// images produced in earlier frames (page-flipped guest framebuffers,
/// mailbox frames filled by a worker thread), so observations must outlive
/// the frame that recorded them; anything untouched for [`STALE_HORIZON`]
/// presents is evicted.
static GENERATION: AtomicU64 = AtomicU64::new(0);
const STALE_HORIZON: u64 = 8;

fn generation() -> u64 {
    GENERATION.load(Ordering::Relaxed)
}

struct Target {
    extent: vk::Extent2D,
    score: u64,
    seen: u64,
}

/// Level-zero extents of live images, fed by the hook's image lifecycle
/// observations.
static IMAGE_EXTENT: LazyLock<DashMap<vk::Image, vk::Extent2D>> = LazyLock::new(Default::default);
/// Images rendered into through framebuffers or filled by transfers.
static TARGETS: LazyLock<DashMap<vk::Image, Target>> = LazyLock::new(Default::default);
/// Which images each render target sampled or was blitted from.
static DEPENDENCIES: LazyLock<DashMap<vk::Image, Vec<(vk::Image, u64)>>> =
    LazyLock::new(Default::default);
/// The most recent layout an image was observed in while being read
/// (descriptor sampling or transfer source): the layout the capture copy
/// must transition from.
static READ_LAYOUT: LazyLock<DashMap<vk::Image, vk::ImageLayout>> =
    LazyLock::new(Default::default);
/// Per swapchain image, cleared when it is presented: what reached it.
/// Blit-presented sources are wholesale frame presentations; draw-sampled
/// sources include incidental overlays such as UI composition quads.
static PRESENTED: LazyLock<DashMap<vk::Image, Vec<(vk::Image, bool)>>> =
    LazyLock::new(Default::default);

/// One selected capture source: a whole presented screen.
#[derive(Clone, Copy)]
pub struct PresentSource {
    pub image: vk::Image,
    pub extent: vk::Extent2D,
    pub layout: vk::ImageLayout,
}

pub fn note_image(image: vk::Image, extent: vk::Extent2D) {
    IMAGE_EXTENT.insert(image, extent);
}

pub fn image_extent(image: vk::Image) -> Option<vk::Extent2D> {
    IMAGE_EXTENT.get(&image).map(|entry| *entry)
}

pub fn forget_image(image: vk::Image) {
    IMAGE_EXTENT.remove(&image);
    TARGETS.remove(&image);
    DEPENDENCIES.remove(&image);
    READ_LAYOUT.remove(&image);
    PRESENTED.remove(&image);
}

pub fn record_target(image: vk::Image, score: u64) {
    if !snowglass_hook::config::graph_tracking_enabled() {
        return;
    }
    let Some(extent) = image_extent(image) else {
        return;
    };
    upsert_target(image, extent, score);
}

fn upsert_target(image: vk::Image, extent: vk::Extent2D, score: u64) {
    let now = generation();
    match TARGETS.entry(image) {
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

pub fn record_dependencies(destination: vk::Image, sampled: &[(vk::Image, vk::ImageLayout)]) {
    if sampled.is_empty() || !snowglass_hook::config::graph_tracking_enabled() {
        return;
    }
    let now = generation();
    let mut inputs = DEPENDENCIES.entry(destination).or_default();
    for &(image, layout) in sampled {
        if image == destination {
            continue;
        }
        READ_LAYOUT.insert(image, layout);
        if let Some((_, seen)) = inputs.iter_mut().find(|(input, _)| *input == image) {
            *seen = now;
        } else {
            inputs.push((image, now));
        }
    }
}

/// An offscreen blit or copy makes the destination a render target fed by
/// the source; display-transfer emulation moves finished frames this way.
pub fn record_transfer(source: vk::Image, source_layout: vk::ImageLayout, destination: vk::Image) {
    if !snowglass_hook::config::graph_tracking_enabled() {
        return;
    }
    let Some(extent) = image_extent(destination) else {
        return;
    };
    READ_LAYOUT.insert(source, source_layout);
    touch(&[source]);
    upsert_target(
        destination,
        extent,
        u64::from(extent.width) * u64::from(extent.height),
    );
    let now = generation();
    let mut inputs = DEPENDENCIES.entry(destination).or_default();
    if let Some((_, seen)) = inputs.iter_mut().find(|(input, _)| *input == source) {
        *seen = now;
    } else {
        inputs.push((source, now));
    }
}

pub fn record_presented(
    swapchain_image: vk::Image,
    sources: &[(vk::Image, vk::ImageLayout)],
    via_blit: bool,
) {
    if sources.is_empty() || !snowglass_hook::config::graph_tracking_enabled() {
        return;
    }
    {
        let mut presented = PRESENTED.entry(swapchain_image).or_default();
        for &(image, layout) in sources {
            READ_LAYOUT.insert(image, layout);
            if let Some((_, blit)) = presented.iter_mut().find(|(source, _)| *source == image) {
                *blit |= via_blit;
            } else {
                presented.push((image, via_blit));
            }
        }
    }
    touch(&sources.iter().map(|(image, _)| *image).collect::<Vec<_>>());
}

/// Refreshes the staleness stamps of images that are still reaching the
/// screen. A static scene re-presents buffers without redrawing them, and
/// their history must not decay while that lasts.
fn touch(images: &[vk::Image]) {
    let now = generation();
    for image in images {
        if let Some(mut target) = TARGETS.get_mut(image) {
            target.seen = now;
        }
    }
}

/// Re-resolves a previous selection against the live stores, dropping
/// images that have been destroyed or aged out so a stale selection cannot
/// outlive the buffers it names. Entries stay live only while the
/// application keeps rendering into them; this deliberately does not refresh
/// their stamps.
pub fn refresh_sources(images: &[vk::Image]) -> Vec<PresentSource> {
    images
        .iter()
        .filter_map(|&image| {
            let target = TARGETS.get(&image)?;
            Some(PresentSource {
                image,
                extent: target.extent,
                layout: READ_LAYOUT
                    .get(&image)
                    .map_or(vk::ImageLayout::GENERAL, |layout| *layout),
            })
        })
        .collect()
}

/// Selects up to `limit` offscreen sources for the presented swapchain
/// image, walking presentation roots through the dependency graph. Each
/// returned source is one whole screen; the caller streams them separately.
pub fn take_present_sources(presented_image: vk::Image, limit: usize) -> Vec<PresentSource> {
    let now = GENERATION.fetch_add(1, Ordering::Relaxed) + 1;
    let horizon = now.saturating_sub(STALE_HORIZON);
    TARGETS.retain(|_, target| target.seen >= horizon);
    DEPENDENCIES.retain(|_, inputs| {
        inputs.retain(|(_, seen)| *seen >= horizon);
        !inputs.is_empty()
    });
    let Some((_, presented)) = PRESENTED.remove(&presented_image) else {
        return Vec::new();
    };
    // A wholesale blit to the swapchain is definitive: its source is the
    // frame even when nothing rendered into it through observed commands,
    // so give such roots a stand-in target if rendering never made one.
    for &(image, via_blit) in &presented {
        if via_blit
            && !TARGETS.contains_key(&image)
            && let Some(extent) = image_extent(image)
        {
            upsert_target(
                image,
                extent,
                u64::from(extent.width) * u64::from(extent.height),
            );
        }
    }
    let dependencies = DEPENDENCIES
        .iter()
        .map(|entry| (*entry.key(), entry.value().clone()))
        .collect::<HashMap<_, _>>();
    let mut blit_roots = presented
        .iter()
        .filter(|(_, via_blit)| *via_blit)
        .map(|(image, _)| *image)
        .collect::<Vec<_>>();
    collapse_flip_pairs(&mut blit_roots, &dependencies);
    let all_roots = presented
        .iter()
        .map(|(image, _)| *image)
        .collect::<Vec<_>>();
    // Trust blit roots exclusively when they resolve: they are exact, and
    // independently blitted screens may differ in extent. Draw-sampled
    // roots keep the extent cohort, which filters UI overlay textures.
    let selected = select(&blit_roots, &dependencies, limit, true);
    let selected = if selected.is_empty() {
        largest_extent_cohort(select(&all_roots, &dependencies, limit, false))
    } else {
        selected
    };
    selected
        .into_iter()
        .filter_map(|image| {
            let target = TARGETS.get(&image)?;
            Some(PresentSource {
                image,
                extent: target.extent,
                layout: READ_LAYOUT
                    .get(&image)
                    .map_or(vk::ImageLayout::GENERAL, |layout| *layout),
            })
        })
        .collect()
}

fn select(
    roots: &[vk::Image],
    dependencies: &HashMap<vk::Image, Vec<(vk::Image, u64)>>,
    limit: usize,
    decompose_roots: bool,
) -> Vec<vk::Image> {
    let area = |image: &vk::Image| {
        TARGETS.get(image).map_or(0, |target| {
            u64::from(target.extent.width) * u64::from(target.extent.height)
        })
    };
    let mut frontier = roots
        .iter()
        .copied()
        .filter(|image| TARGETS.contains_key(image))
        .collect::<Vec<_>>();
    deduplicate(&mut frontier);
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
            .any(|other| other != image && reachable(*other, *image, dependencies))
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
                .filter(|input| input != image && TARGETS.contains_key(input))
                .collect::<Vec<_>>();
            if inputs.is_empty() {
                return None;
            }
            // A composite redrawn every few presents accumulates both
            // faces of each page-flipped screen it samples; keep only the
            // freshest face of each before deciding to decompose.
            collapse_flip_pairs(&mut inputs, dependencies);
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
        deduplicate(&mut frontier);
    }
    // The recorded order varies with which mailbox frame happened to be
    // presented, and a capture that reorders its screens every frame
    // reads as flicker. Order deterministically instead: tallest and
    // widest first (a 3DS top screen before the bottom one), with the
    // stable sort preserving arrival order between equals.
    frontier.sort_by_key(|image| {
        TARGETS.get(image).map_or((0, 0), |target| {
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
                .filter(|(input, _)| TARGETS.contains_key(input))
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
    roots: &mut Vec<vk::Image>,
    dependencies: &HashMap<vk::Image, Vec<(vk::Image, u64)>>,
) {
    let extent_of = |image: &vk::Image| {
        TARGETS
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
fn largest_extent_cohort(selected: Vec<vk::Image>) -> Vec<vk::Image> {
    let extent_of = |image: &vk::Image| {
        TARGETS
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

static RENDER_TARGET: LazyLock<DashMap<vk::Image, RenderTargetCandidate>> =
    LazyLock::new(Default::default);
static IMAGE_INFO: LazyLock<DashMap<vk::Image, (vk::Format, vk::ImageUsageFlags)>> =
    LazyLock::new(Default::default);
static IMAGE_VIEW: LazyLock<DashMap<vk::ImageView, vk::Image>> = LazyLock::new(Default::default);
static COLOR_IMAGE_VIEW: LazyLock<DashMap<vk::ImageView, vk::Image>> =
    LazyLock::new(Default::default);
static FRAMEBUFFER_IMAGES: LazyLock<DashMap<vk::Framebuffer, Vec<vk::Image>>> =
    LazyLock::new(Default::default);
static DESCRIPTOR_SET_IMAGES: LazyLock<
    DashMap<vk::DescriptorSet, HashMap<(u32, u32), (vk::Image, vk::ImageLayout)>>,
> = LazyLock::new(Default::default);
static IMAGE_COHORT: LazyLock<DashMap<(i32, u32, u32, u32), u32>> = LazyLock::new(Default::default);
static COMMAND_TRACKING: LazyLock<DashMap<vk::CommandBuffer, CommandTrackingState>> =
    LazyLock::new(Default::default);
static COMMAND_CANDIDATE: LazyLock<DashMap<vk::CommandBuffer, (vk::Image, RenderTargetCandidate)>> =
    LazyLock::new(Default::default);
static SWAPCHAIN_IMAGES: LazyLock<DashMap<vk::Image, ()>> = LazyLock::new(Default::default);
/// Per presented image: the raster rect its content was drawn with, kept for
/// the strongest draw. This is the scissor-derived crop of a presentation.
static PRESENT_RECT: LazyLock<DashMap<vk::Image, (vk::Rect2D, u64)>> =
    LazyLock::new(Default::default);

pub fn command_candidate(
    command_buffer: vk::CommandBuffer,
) -> Option<(vk::Image, RenderTargetCandidate)> {
    COMMAND_CANDIDATE.get(&command_buffer).map(|entry| *entry)
}

pub fn render_target(image: vk::Image) -> Option<RenderTargetCandidate> {
    RENDER_TARGET.get(&image).map(|entry| *entry)
}

pub fn remove_render_target(image: vk::Image) {
    RENDER_TARGET.remove(&image);
}

pub fn swapchain_image_added(image: vk::Image) {
    SWAPCHAIN_IMAGES.insert(image, ());
}

pub fn swapchain_image_removed(image: vk::Image) {
    SWAPCHAIN_IMAGES.remove(&image);
    RENDER_TARGET.remove(&image);
    PRESENT_RECT.remove(&image);
}

pub fn is_swapchain_image(image: vk::Image) -> bool {
    SWAPCHAIN_IMAGES.contains_key(&image)
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

fn record_present_rect(image: vk::Image, rect: vk::Rect2D, draw_area: u64) {
    let replace = PRESENT_RECT
        .get(&image)
        .is_none_or(|current| draw_area >= current.1);
    if replace {
        PRESENT_RECT.insert(image, (rect, draw_area));
    }
}

pub fn capture_rect(image: vk::Image, bounds: vk::Extent2D) -> Option<vk::Rect2D> {
    snowglass_hook::config::tracking_enabled()
        .then(|| PRESENT_RECT.get(&image).map(|entry| entry.0))
        .flatten()
        .and_then(|rect| clamp_rect(rect, bounds))
}

pub fn image_created(
    image: vk::Image,
    extent: vk::Extent2D,
    format: vk::Format,
    usage: vk::ImageUsageFlags,
) {
    note_image(image, extent);
    let usage = usage | vk::ImageUsageFlags::TRANSFER_SRC;
    IMAGE_INFO.insert(image, (format, usage));
    IMAGE_COHORT
        .entry((format.as_raw(), usage.as_raw(), extent.width, extent.height))
        .and_modify(|count| *count += 1)
        .or_insert(1);
}

pub fn image_destroyed(image: vk::Image) {
    let cohort_key = IMAGE_INFO.get(&image).and_then(|info| {
        image_extent(image).map(|extent| (info.0.as_raw(), info.1.as_raw(), extent.width, extent.height))
    });
    IMAGE_INFO.remove(&image);
    PRESENT_RECT.remove(&image);
    RENDER_TARGET.remove(&image);
    forget_image(image);
    if let Some(key) = cohort_key
        && let Some(mut count) = IMAGE_COHORT.get_mut(&key)
    {
        *count = count.saturating_sub(1);
        let remove = *count == 0;
        drop(count);
        if remove {
            IMAGE_COHORT.remove(&key);
        }
    }
}

pub fn image_view_created(view: vk::ImageView, image: vk::Image, color: bool) {
    IMAGE_VIEW.insert(view, image);
    if color {
        COLOR_IMAGE_VIEW.insert(view, image);
    }
}

pub fn image_view_destroyed(view: vk::ImageView) {
    IMAGE_VIEW.remove(&view);
    COLOR_IMAGE_VIEW.remove(&view);
}

pub fn framebuffer_created(framebuffer: vk::Framebuffer, attachments: &[vk::ImageView]) {
    let images = attachments
        .iter()
        .filter_map(|view| COLOR_IMAGE_VIEW.get(view).map(|entry| *entry))
        .collect();
    FRAMEBUFFER_IMAGES.insert(framebuffer, images);
}

pub fn framebuffer_destroyed(framebuffer: vk::Framebuffer) {
    FRAMEBUFFER_IMAGES.remove(&framebuffer);
}

pub fn descriptor_images_written(
    set: vk::DescriptorSet,
    entries: &[(u32, u32, vk::ImageView, vk::ImageLayout)],
) {
    for &(binding, element, view, layout) in entries {
        if let Some(image) = IMAGE_VIEW.get(&view).map(|entry| *entry) {
            DESCRIPTOR_SET_IMAGES
                .entry(set)
                .or_default()
                .insert((binding, element), (image, layout));
        }
    }
}

pub fn command_buffer_began(command_buffer: vk::CommandBuffer) {
    COMMAND_TRACKING.remove(&command_buffer);
    COMMAND_CANDIDATE.remove(&command_buffer);
}

pub fn command_bound_descriptor_sets(
    command_buffer: vk::CommandBuffer,
    first_set: u32,
    sets: &[vk::DescriptorSet],
) {
    if sets.is_empty() {
        return;
    }
    let mut state = COMMAND_TRACKING.entry(command_buffer).or_default();
    let required = first_set as usize + sets.len();
    state
        .descriptor_sets
        .resize(required, vk::DescriptorSet::null());
    state.descriptor_sets[first_set as usize..required].copy_from_slice(sets);
    state.dirty = true;
}

pub fn command_pushed_descriptors(
    command_buffer: vk::CommandBuffer,
    views: &[(vk::ImageView, vk::ImageLayout)],
) {
    let pushed_images = views
        .iter()
        .filter_map(|&(view, layout)| IMAGE_VIEW.get(&view).map(|entry| (*entry, layout)))
        .collect();
    let mut state = COMMAND_TRACKING.entry(command_buffer).or_default();
    state.pushed_images = pushed_images;
    state.dirty = true;
}

pub fn command_set_viewport(command_buffer: vk::CommandBuffer, viewport: vk::Viewport) {
    let mut state = COMMAND_TRACKING.entry(command_buffer).or_default();
    state.viewport = Some(viewport);
    state.dirty = true;
}

pub fn command_set_scissor(command_buffer: vk::CommandBuffer, scissor: vk::Rect2D) {
    let mut state = COMMAND_TRACKING.entry(command_buffer).or_default();
    state.scissor = Some(scissor);
    state.dirty = true;
}

pub fn command_began_render_pass(command_buffer: vk::CommandBuffer, framebuffer: vk::Framebuffer) {
    let mut state = COMMAND_TRACKING.entry(command_buffer).or_default();
    state.framebuffer = Some(framebuffer);
    state.dirty = true;
}

pub fn command_ended_render_pass(command_buffer: vk::CommandBuffer) {
    if let Some(mut state) = COMMAND_TRACKING.get_mut(&command_buffer) {
        state.framebuffer = None;
        state.dirty = true;
    }
}

fn image_cohort_size(image: vk::Image, extent: vk::Extent2D) -> u32 {
    let Some(info) = IMAGE_INFO.get(&image).map(|entry| *entry) else {
        return 0;
    };
    IMAGE_COHORT
        .get(&(info.0.as_raw(), info.1.as_raw(), extent.width, extent.height))
        .map_or(0, |entry| *entry)
}

/// The draw observer. Feeds both the one-hop candidate heuristic (default)
/// and the dependency graph (opt-in).
pub fn command_drew(command_buffer: vk::CommandBuffer) {
    let Some(mut state) = COMMAND_TRACKING.get_mut(&command_buffer) else {
        return;
    };
    if !state.dirty {
        return;
    }
    state.dirty = false;
    let Some(framebuffer) = state.framebuffer else {
        return;
    };
    let Some(images) = FRAMEBUFFER_IMAGES.get(&framebuffer) else {
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
        let Some(extent) = image_extent(image) else {
            return;
        };
        let pixels = u64::from(extent.width) * u64::from(extent.height);
        if pixels < 64 * 64 {
            return;
        }
        let cohort = image_cohort_size(image, extent);
        if best.is_none_or(|candidate: RenderTargetCandidate| {
            cohort > candidate.cohort_size
                || (cohort == candidate.cohort_size
                    && pixels
                        > u64::from(candidate.extent.width) * u64::from(candidate.extent.height))
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
        let Some(images) = DESCRIPTOR_SET_IMAGES.get(&set) else {
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
        image_extent(*image)
            .is_some_and(|extent| u64::from(extent.width) * u64::from(extent.height) >= 64 * 64)
    });
    if is_swapchain_image(destination) {
        record_presented(destination, &sampled, false);
    } else {
        record_target(destination, draw_area);
        record_dependencies(destination, &sampled);
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
            record_present_rect(destination, rect, draw_area);
        }
        let replace = RENDER_TARGET
            .get(&destination)
            .is_none_or(|old| candidate.is_better_than(*old));
        if replace {
            RENDER_TARGET.insert(destination, candidate);
        }
        let replace_command = COMMAND_CANDIDATE
            .get(&command_buffer)
            .is_none_or(|old| candidate.is_better_than(old.1));
        if replace_command {
            COMMAND_CANDIDATE.insert(command_buffer, (destination, candidate));
        }
    }
}

fn propagate_present_rect(
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
    let Some(source_bounds) = image_extent(source_image) else {
        return;
    };
    let Some(rect) = capture_rect(source_image, source_bounds) else {
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
        record_present_rect(destination_image, mapped, area);
    }
}

/// Blit observer: an offscreen destination joins the dependency graph, a
/// swapchain destination is a trusted presentation.
pub fn command_blitted(
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
    if !is_swapchain_image(destination_image) {
        record_transfer(source_image, source_layout, destination_image);
        return;
    }
    record_presented(destination_image, &[(source_image, source_layout)], true);
    if source_extent.width != 0 && source_extent.height != 0 {
        propagate_present_rect(
            source_image,
            destination_image,
            source_offset,
            source_extent,
            destination_offset,
            destination_extent,
        );
        RENDER_TARGET.insert(
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
