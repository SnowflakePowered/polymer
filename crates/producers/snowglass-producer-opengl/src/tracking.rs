//! Heuristics for finding the game texture and its presented bounds.

use crate::interop::{
    GL_ACTIVE_TEXTURE, GL_DRAW_FRAMEBUFFER, GL_DRAW_FRAMEBUFFER_BINDING, GL_READ_FRAMEBUFFER,
    GL_READ_FRAMEBUFFER_BINDING, GL_SCISSOR_BOX, GL_SCISSOR_TEST, GL_TEXTURE_2D,
    GL_TEXTURE_2D_ARRAY, GL_TEXTURE_BINDING_2D, GL_TEXTURE_BINDING_2D_ARRAY, GL_TEXTURE_HEIGHT,
    GL_TEXTURE_WIDTH, GL_TEXTURE0, GL_VIEWPORT, GlActiveTexture, Glenum, Gluint, current_context,
    glBindTexture, glGetIntegerv, glGetTexLevelParameteriv, glIsEnabled, proc,
};
use std::{
    collections::{HashMap, HashSet},
    sync::{
        Mutex, OnceLock,
        atomic::{AtomicU64, Ordering},
    },
};

static GENERATION: AtomicU64 = AtomicU64::new(0);
const STALE_HORIZON: u64 = 8;

fn generation() -> u64 {
    GENERATION.load(Ordering::Relaxed)
}

#[derive(Clone, Copy, Debug)]
pub(crate) struct QuadBounds {
    pub(crate) x: u32,
    pub(crate) y: u32,
    pub(crate) width: u32,
    pub(crate) height: u32,
}

#[derive(Clone, Copy)]
pub(crate) struct DrawCandidate {
    pub(crate) texture: Gluint,
    pub(crate) target: Glenum,
    pub(crate) layer: i32,
    pub(crate) width: u32,
    pub(crate) height: u32,
    pub(crate) bounds: QuadBounds,
    score: u64,
}

#[derive(Clone, Copy, Debug, Eq, Hash, PartialEq)]
struct TargetKey {
    texture: Gluint,
    layer: i32,
}

impl DrawCandidate {
    fn key(self) -> TargetKey {
        TargetKey {
            texture: self.texture,
            layer: self.layer,
        }
    }
}

/// Per-context, per-frame: the strongest texture sampled by draws into the
/// default framebuffer. Only used when no render-target graph exists.
static DRAW_CANDIDATES: OnceLock<Mutex<HashMap<usize, Vec<DrawCandidate>>>> = OnceLock::new();
/// Textures rendered into through framebuffer objects, by draws or blits.
/// Texture names are share-group scoped, so a single store lets a presenting
/// context resolve targets produced on a worker context.
static RENDER_TARGETS: OnceLock<Mutex<Vec<(DrawCandidate, u64)>>> = OnceLock::new();
static TEXTURE_INFO: OnceLock<Mutex<HashMap<Gluint, (Glenum, u32, u32)>>> = OnceLock::new();
/// Which textures each render target sampled or blitted from while being filled.
static DEPENDENCIES: OnceLock<Mutex<HashMap<TargetKey, Vec<(Gluint, u64)>>>> = OnceLock::new();
/// Per-context, per-frame: textures that reached the default framebuffer.
/// Blit-presented textures are wholesale frame presentations; draw-sampled
/// textures include incidental overlays such as UI composition quads.
static PRESENTED_TEXTURES: OnceLock<Mutex<HashMap<usize, Vec<(Gluint, bool)>>>> = OnceLock::new();
/// Renderbuffers cannot be sampled, so a renderbuffer that reaches the screen
/// stands in for the textures that were drawn or blitted into it.
static RENDERBUFFER_INPUTS: OnceLock<Mutex<HashMap<Gluint, Vec<(Gluint, u64)>>>> = OnceLock::new();
static ACTIVE_TEXTURE: OnceLock<GlActiveTexture> = OnceLock::new();
type GlGetFramebufferAttachmentParameteriv =
    unsafe extern "system" fn(Glenum, Glenum, Glenum, *mut i32);
static GET_FRAMEBUFFER_ATTACHMENT: OnceLock<GlGetFramebufferAttachmentParameteriv> =
    OnceLock::new();

const GL_FRAMEBUFFER_ATTACHMENT_OBJECT_TYPE: Glenum = 0x8CD0;
const GL_FRAMEBUFFER_ATTACHMENT_OBJECT_NAME: Glenum = 0x8CD1;
const GL_FRAMEBUFFER_ATTACHMENT_TEXTURE_LAYER: Glenum = 0x8CD4;
const GL_TEXTURE: Glenum = 0x1702;
const GL_RENDERBUFFER: Glenum = 0x8D41;
const GL_MAX_DRAW_BUFFERS: Glenum = 0x8824;
const GL_DRAW_BUFFER0: Glenum = 0x8825;

fn draw_candidates() -> &'static Mutex<HashMap<usize, Vec<DrawCandidate>>> {
    DRAW_CANDIDATES.get_or_init(|| Mutex::new(HashMap::new()))
}

fn render_targets() -> &'static Mutex<Vec<(DrawCandidate, u64)>> {
    RENDER_TARGETS.get_or_init(|| Mutex::new(Vec::new()))
}

fn texture_info() -> &'static Mutex<HashMap<Gluint, (Glenum, u32, u32)>> {
    TEXTURE_INFO.get_or_init(|| Mutex::new(HashMap::new()))
}

fn dependencies() -> &'static Mutex<HashMap<TargetKey, Vec<(Gluint, u64)>>> {
    DEPENDENCIES.get_or_init(|| Mutex::new(HashMap::new()))
}

fn presented_textures() -> &'static Mutex<HashMap<usize, Vec<(Gluint, bool)>>> {
    PRESENTED_TEXTURES.get_or_init(|| Mutex::new(HashMap::new()))
}

fn renderbuffer_inputs() -> &'static Mutex<HashMap<Gluint, Vec<(Gluint, u64)>>> {
    RENDERBUFFER_INPUTS.get_or_init(|| Mutex::new(HashMap::new()))
}

fn upsert_render_target(candidate: DrawCandidate) {
    let now = generation();
    let mut targets = render_targets()
        .lock()
        .unwrap_or_else(|error| error.into_inner());
    if let Some((existing, seen)) = targets
        .iter_mut()
        .find(|(existing, _)| existing.key() == candidate.key())
    {
        if candidate.score >= existing.score || *seen + STALE_HORIZON < now {
            *existing = candidate;
        }
        *seen = now;
    } else {
        targets.push((candidate, now));
    }
}

fn record_dependencies(key: TargetKey, sources: &[Gluint]) {
    let now = generation();
    let mut graphs = dependencies()
        .lock()
        .unwrap_or_else(|error| error.into_inner());
    let inputs = graphs.entry(key).or_default();
    for &source in sources {
        if source == key.texture {
            continue;
        }
        if let Some((_, seen)) = inputs.iter_mut().find(|(texture, _)| *texture == source) {
            *seen = now;
        } else {
            inputs.push((source, now));
        }
    }
}

fn record_renderbuffer_inputs(renderbuffer: Gluint, sources: &[Gluint]) {
    let now = generation();
    let mut all_inputs = renderbuffer_inputs()
        .lock()
        .unwrap_or_else(|error| error.into_inner());
    let inputs = all_inputs.entry(renderbuffer).or_default();
    for &source in sources {
        if let Some((_, seen)) = inputs.iter_mut().find(|(texture, _)| *texture == source) {
            *seen = now;
        } else {
            inputs.push((source, now));
        }
    }
}

fn record_presented(context: usize, sources: &[Gluint], via_blit: bool) {
    {
        let mut presented = presented_textures()
            .lock()
            .unwrap_or_else(|error| error.into_inner());
        let presented = presented.entry(context).or_default();
        for &source in sources {
            if let Some((_, blit)) = presented.iter_mut().find(|(texture, _)| *texture == source) {
                *blit |= via_blit;
            } else {
                presented.push((source, via_blit));
            }
        }
    }
    touch_render_targets(sources);
}

/// Refreshes the staleness stamps of render targets that are still reaching
/// the screen. A static scene re-presents buffers without redrawing them, and
/// their history must not decay while that lasts.
fn touch_render_targets(sources: &[Gluint]) {
    let now = generation();
    let mut targets = render_targets()
        .lock()
        .unwrap_or_else(|error| error.into_inner());
    for (candidate, seen) in targets.iter_mut() {
        if sources.contains(&candidate.texture) {
            *seen = now;
        }
    }
}

fn intersect(a: [i32; 4], b: [i32; 4]) -> Option<[i32; 4]> {
    let left = a[0].max(b[0]);
    let bottom = a[1].max(b[1]);
    let right = a[0].saturating_add(a[2]).min(b[0].saturating_add(b[2]));
    let top = a[1].saturating_add(a[3]).min(b[1].saturating_add(b[3]));
    (right > left && top > bottom).then_some([left, bottom, right - left, top - bottom])
}

/// Returns the selected capture sources and whether they came from a trusted
/// blit-presented graph resolution. Trusted selections are complete: the
/// caller must not pad them from history.
pub(crate) fn take_draw_candidates(context: usize) -> (Vec<DrawCandidate>, bool) {
    let Some(limit) = tracked_render_target_count() else {
        return (Vec::new(), false);
    };
    let now = GENERATION.fetch_add(1, Ordering::Relaxed) + 1;
    let horizon = now.saturating_sub(STALE_HORIZON);
    let fallback = draw_candidates()
        .lock()
        .unwrap_or_else(|error| error.into_inner())
        .remove(&context)
        .unwrap_or_default();
    let presented = presented_textures()
        .lock()
        .unwrap_or_else(|error| error.into_inner())
        .remove(&context)
        .unwrap_or_default();
    let mut targets = {
        let mut store = render_targets()
            .lock()
            .unwrap_or_else(|error| error.into_inner());
        store.retain(|(_, seen)| *seen >= horizon);
        store
            .iter()
            .map(|(candidate, _)| *candidate)
            .collect::<Vec<_>>()
    };
    {
        let info = texture_info()
            .lock()
            .unwrap_or_else(|error| error.into_inner());
        targets.retain_mut(|candidate| {
            let Some(&(target, width, height)) = info.get(&candidate.texture) else {
                return false;
            };
            candidate.target = target;
            candidate.width = width;
            candidate.height = height;
            true
        });
    }
    let dependency_graph = {
        let mut graphs = dependencies()
            .lock()
            .unwrap_or_else(|error| error.into_inner());
        graphs.retain(|_, inputs| {
            inputs.retain(|(_, seen)| *seen >= horizon);
            !inputs.is_empty()
        });
        graphs.clone()
    };
    renderbuffer_inputs()
        .lock()
        .unwrap_or_else(|error| error.into_inner())
        .retain(|_, inputs| {
            inputs.retain(|(_, seen)| *seen >= horizon);
            !inputs.is_empty()
        });
    if !graph_tracking_enabled() {
        return (
            select_screen_cohort(
                if targets.is_empty() { fallback } else { targets },
                limit,
            ),
            false,
        );
    }
    // A blit into the default framebuffer presents a whole frame, while
    // draws into it also cover UI overlays sampling their own backing
    // textures. When blit roots resolve, trust them exclusively: they are
    // exact, and independently blitted screens may legitimately differ in
    // extent (a 3DS top screen is wider than the bottom one), so the extent
    // cohort must not prune them. Draw-sampled roots keep the cohort filter.
    let mut blit_roots = presented
        .iter()
        .filter(|(_, via_blit)| *via_blit)
        .map(|(texture, _)| *texture)
        .collect::<Vec<_>>();
    let all_roots = presented
        .iter()
        .map(|(texture, _)| *texture)
        .collect::<Vec<_>>();
    // A wholesale blit to the screen is definitive: its source is the frame
    // even when it was filled by CPU uploads that no draw or blit observed,
    // so give such roots a stand-in candidate if rendering never made one.
    {
        let info = texture_info()
            .lock()
            .unwrap_or_else(|error| error.into_inner());
        for &root in &blit_roots {
            if targets.iter().any(|candidate| candidate.texture == root) {
                continue;
            }
            let Some(&(target, width, height)) = info.get(&root) else {
                continue;
            };
            targets.push(DrawCandidate {
                texture: root,
                target,
                layer: 0,
                width,
                height,
                bounds: QuadBounds {
                    x: 0,
                    y: 0,
                    width,
                    height,
                },
                score: u64::from(width) * u64::from(height),
            });
        }
    }
    collapse_flip_pairs(&mut blit_roots, &targets, &dependency_graph);
    let mut trusted = false;
    let mut graph_selected = Vec::new();
    if !blit_roots.is_empty() {
        graph_selected =
            select_from_dependency_graph(&targets, &dependency_graph, &blit_roots, limit);
        trusted = !graph_selected.is_empty();
    }
    if graph_selected.is_empty() {
        graph_selected =
            select_from_dependency_graph(&targets, &dependency_graph, &all_roots, limit);
    }
    if trusted {
        // Trusted selections are whole presented screens. A game may draw
        // directly into a display buffer with an interior scissor, and the
        // per-draw bounds recorded from that must not crop the capture.
        for candidate in &mut graph_selected {
            candidate.bounds = QuadBounds {
                x: 0,
                y: 0,
                width: candidate.width,
                height: candidate.height,
            };
        }
        return (graph_selected, true);
    }
    if !graph_selected.is_empty() {
        return (select_screen_cohort(graph_selected, limit), false);
    }
    (
        select_screen_cohort(
            if targets.is_empty() {
                fallback
            } else {
                targets
            },
            limit,
        ),
        false,
    )
}

/// Page-flipped guest framebuffers alternate two textures for one screen, and
/// both faces can reach the screen within one capture interval. Two roots with
/// the same extent that were filled from a shared source are one logical
/// screen; the one whose shared edge was refreshed most recently survives.
fn collapse_flip_pairs(
    roots: &mut Vec<Gluint>,
    candidates: &[DrawCandidate],
    dependencies: &HashMap<TargetKey, Vec<(Gluint, u64)>>,
) {
    let extent_of = |texture: Gluint| {
        candidates
            .iter()
            .find(|candidate| candidate.texture == texture)
            .map(|candidate| (candidate.width, candidate.height))
    };
    let inputs_of = |texture: Gluint| {
        let mut inputs = HashMap::<Gluint, u64>::new();
        for (key, edges) in dependencies {
            if key.texture != texture {
                continue;
            }
            for &(source, seen) in edges {
                let entry = inputs.entry(source).or_insert(seen);
                *entry = (*entry).max(seen);
            }
        }
        inputs
    };
    let mut removed = HashSet::new();
    let snapshot = roots.clone();
    for (index, &first) in snapshot.iter().enumerate() {
        for &second in &snapshot[index + 1..] {
            if removed.contains(&first) || removed.contains(&second) {
                continue;
            }
            let extent = extent_of(first);
            if extent.is_none() || extent != extent_of(second) {
                continue;
            }
            let first_inputs = inputs_of(first);
            let second_inputs = inputs_of(second);
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

fn select_from_dependency_graph(
    candidates: &[DrawCandidate],
    dependencies: &HashMap<TargetKey, Vec<(Gluint, u64)>>,
    presented: &[Gluint],
    limit: usize,
) -> Vec<DrawCandidate> {
    let by_key = candidates
        .iter()
        .copied()
        .map(|candidate| (candidate.key(), candidate))
        .collect::<HashMap<_, _>>();
    let nodes_for_texture = |texture: Gluint| {
        candidates
            .iter()
            .filter(move |candidate| candidate.texture == texture)
            .map(|candidate| candidate.key())
            .collect::<Vec<_>>()
    };
    let mut frontier = presented
        .iter()
        .flat_map(|texture| nodes_for_texture(*texture))
        .collect::<Vec<_>>();
    deduplicate(&mut frontier);
    if frontier.is_empty() {
        return Vec::new();
    }

    // If both an intermediate and one of its inputs are directly sampled by
    // presentation draws, retain the downstream node. Expanding it below will
    // recover all independent inputs without double-counting the same branch.
    let roots = frontier.clone();
    frontier.retain(|candidate| {
        !roots.iter().any(|other| {
            other != candidate
                && reachable_texture(*other, candidate.texture, dependencies, candidates)
        })
    });

    while frontier.len() < limit {
        let Some((index, inputs)) = frontier.iter().enumerate().find_map(|(index, key)| {
            let inputs = dependencies
                .get(key)?
                .iter()
                .flat_map(|(texture, _)| nodes_for_texture(*texture))
                .filter(|input| *input != *key)
                .collect::<Vec<_>>();
            if inputs.is_empty() {
                return None;
            }
            // Expanding decomposes a composite into the screens drawn into
            // it. A single input of equal or larger extent is a copy or
            // scaling chain, not a composition; descending it would trade
            // the presented frame for the pipeline that produced it.
            let area = |key: &TargetKey| {
                by_key
                    .get(key)
                    .map(|candidate| u64::from(candidate.width) * u64::from(candidate.height))
                    .unwrap_or(0)
            };
            let distinct = inputs
                .iter()
                .map(|input| input.texture)
                .collect::<HashSet<_>>();
            if distinct.len() < 2 && inputs.iter().all(|input| area(input) >= area(key)) {
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
    frontier
        .into_iter()
        .filter_map(|key| by_key.get(&key).copied())
        .take(limit)
        .collect()
}

fn reachable_texture(
    start: TargetKey,
    texture: Gluint,
    dependencies: &HashMap<TargetKey, Vec<(Gluint, u64)>>,
    candidates: &[DrawCandidate],
) -> bool {
    let mut pending = vec![start];
    let mut visited = HashSet::new();
    while let Some(node) = pending.pop() {
        if !visited.insert(node) {
            continue;
        }
        let Some(inputs) = dependencies.get(&node) else {
            continue;
        };
        if inputs.iter().any(|(input, _)| *input == texture) {
            return true;
        }
        pending.extend(
            candidates
                .iter()
                .filter(|candidate| inputs.iter().any(|(input, _)| *input == candidate.texture))
                .map(|candidate| candidate.key()),
        );
    }
    false
}

fn deduplicate(values: &mut Vec<TargetKey>) {
    let mut seen = HashSet::new();
    values.retain(|value| seen.insert(*value));
}

/// Whether tracked render targets resolve through the presentation
/// dependency graph. Off by default: the graph digs out offscreen screens
/// behind compositor blits and copies (3DS-style emulators) but its
/// decomposition heuristics can misfire on games with unusual sampling
/// patterns, so the cohort heuristics remain the default.
pub(crate) fn graph_tracking_enabled() -> bool {
    static ENABLED: OnceLock<bool> = OnceLock::new();
    *ENABLED.get_or_init(|| std::env::var_os("SNOWGLASS_GRAPH_TRACKING").is_some())
}

pub(crate) fn tracked_render_target_count() -> Option<usize> {
    if let Some(value) = std::env::var_os("SNOWGLASS_TRACK_RENDER_TARGETS") {
        return value
            .to_str()?
            .parse::<usize>()
            .ok()
            .filter(|count| *count != 0);
    }
    std::env::var_os("SNOWGLASS_TRACK_RENDER_TARGET").map(|_| 1)
}

/// Keeps the largest group of distinct textures with a common extent. Games
/// commonly draw a screen using multiple primitives, so repeated observations
/// of one texture are collapsed. Grouping by extent filters incidental UI and
/// font textures while retaining independently rendered, same-sized screens.
fn select_screen_cohort(candidates: Vec<DrawCandidate>, limit: usize) -> Vec<DrawCandidate> {
    let mut distinct = Vec::<DrawCandidate>::new();
    for candidate in candidates {
        if let Some(existing) = distinct.iter_mut().find(|existing| {
            (existing.texture, existing.target, existing.layer)
                == (candidate.texture, candidate.target, candidate.layer)
        }) {
            if candidate.score > existing.score {
                *existing = candidate;
            }
        } else {
            distinct.push(candidate);
        }
    }
    let Some(best_extent) = distinct
        .iter()
        .map(|candidate| (candidate.width, candidate.height))
        .max_by_key(|&(width, height)| {
            let matching = distinct
                .iter()
                .filter(|candidate| (candidate.width, candidate.height) == (width, height));
            let count = matching.clone().count();
            let score = matching.map(|candidate| candidate.score).max().unwrap_or(0);
            (
                count.min(limit),
                score,
                u64::from(width) * u64::from(height),
            )
        })
    else {
        return Vec::new();
    };
    distinct.retain(|candidate| (candidate.width, candidate.height) == best_extent);
    distinct.truncate(limit);
    distinct
}

/// Records sampled textures, framebuffer destinations, and the draw's raster bounds.
pub unsafe fn record_draw_candidate() {
    if tracked_render_target_count().is_none() {
        return;
    }
    let context = unsafe { current_context() };
    if context.is_null() {
        return;
    }
    let mut draw_framebuffer = 0;
    unsafe { glGetIntegerv(GL_DRAW_FRAMEBUFFER_BINDING, &mut draw_framebuffer) };
    let active_texture = if let Some(function) = ACTIVE_TEXTURE.get() {
        *function
    } else {
        let Some(function): Option<GlActiveTexture> = (unsafe { proc(b"glActiveTexture\0") })
        else {
            return;
        };
        let _ = ACTIVE_TEXTURE.set(function);
        function
    };
    let mut viewport = [0; 4];
    let mut old_active = 0;
    unsafe {
        glGetIntegerv(GL_VIEWPORT, viewport.as_mut_ptr());
        glGetIntegerv(GL_ACTIVE_TEXTURE, &mut old_active);
    }
    let raster = if unsafe { glIsEnabled(GL_SCISSOR_TEST) } != 0 {
        let mut scissor = [0; 4];
        unsafe { glGetIntegerv(GL_SCISSOR_BOX, scissor.as_mut_ptr()) };
        intersect(viewport, scissor)
    } else {
        (viewport[2] > 0 && viewport[3] > 0).then_some(viewport)
    };
    let Some(raster) = raster else { return };
    let bounds = QuadBounds {
        x: raster[0].max(0) as u32,
        y: raster[1].max(0) as u32,
        width: raster[2] as u32,
        height: raster[3] as u32,
    };
    let raster_area = u64::from(bounds.width) * u64::from(bounds.height);
    let mut sampled = Vec::<DrawCandidate>::new();
    let active_unit = (old_active as u32).saturating_sub(GL_TEXTURE0);
    for unit in [active_unit, 0]
        .into_iter()
        .enumerate()
        .filter_map(|(index, unit)| (index == 0 || unit != active_unit).then_some(unit))
    {
        unsafe { active_texture(GL_TEXTURE0 + unit) };
        for (target, binding) in [
            (GL_TEXTURE_2D, GL_TEXTURE_BINDING_2D),
            (GL_TEXTURE_2D_ARRAY, GL_TEXTURE_BINDING_2D_ARRAY),
        ] {
            let mut texture = 0;
            let mut width = 0;
            let mut height = 0;
            unsafe {
                glGetIntegerv(binding, &mut texture);
                if texture != 0 {
                    glGetTexLevelParameteriv(target, 0, GL_TEXTURE_WIDTH, &mut width);
                    glGetTexLevelParameteriv(target, 0, GL_TEXTURE_HEIGHT, &mut height);
                }
            }
            if width <= 1 || height <= 1 {
                continue;
            }
            let cross_a = width as u64 * u64::from(bounds.height);
            let cross_b = height as u64 * u64::from(bounds.width);
            let aspect_match = cross_a
                .min(cross_b)
                .saturating_mul(1024)
                .checked_div(cross_a.max(cross_b))
                .unwrap_or(0);
            sampled.push(DrawCandidate {
                texture: texture as u32,
                target,
                layer: 0,
                width: width as u32,
                height: height as u32,
                bounds,
                score: raster_area
                    .saturating_mul(width as u64 * height as u64)
                    .saturating_mul(aspect_match),
            });
            texture_info()
                .lock()
                .unwrap_or_else(|error| error.into_inner())
                .insert(texture as u32, (target, width as u32, height as u32));
        }
    }
    unsafe { active_texture(old_active as u32) };
    let sampled_names = sampled
        .iter()
        .map(|candidate| candidate.texture)
        .collect::<Vec<_>>();
    if draw_framebuffer != 0 {
        let Some(function) = framebuffer_attachment_query() else {
            return;
        };
        let mut draw_buffer_count = 0;
        unsafe { glGetIntegerv(GL_MAX_DRAW_BUFFERS, &mut draw_buffer_count) };
        // A draw can populate several independent render targets at once. Query
        // the enabled draw-buffer routing instead of assuming COLOR_ATTACHMENT0;
        // array layers attached to separate draw buffers are distinct nodes.
        for index in 0..draw_buffer_count.clamp(1, 16) {
            let mut attachment = 0;
            let mut object_type = 0;
            let mut texture = 0;
            let mut layer = 0;
            unsafe {
                glGetIntegerv(GL_DRAW_BUFFER0 + index as u32, &mut attachment);
            }
            if attachment < crate::interop::GL_COLOR_ATTACHMENT0 as i32 {
                continue;
            }
            unsafe {
                function(
                    GL_DRAW_FRAMEBUFFER,
                    attachment as u32,
                    GL_FRAMEBUFFER_ATTACHMENT_OBJECT_TYPE,
                    &mut object_type,
                );
                function(
                    GL_DRAW_FRAMEBUFFER,
                    attachment as u32,
                    GL_FRAMEBUFFER_ATTACHMENT_OBJECT_NAME,
                    &mut texture,
                );
                function(
                    GL_DRAW_FRAMEBUFFER,
                    attachment as u32,
                    GL_FRAMEBUFFER_ATTACHMENT_TEXTURE_LAYER,
                    &mut layer,
                );
            }
            if texture <= 0 {
                continue;
            }
            if object_type == GL_RENDERBUFFER as i32 {
                record_renderbuffer_inputs(texture as u32, &sampled_names);
                continue;
            }
            if object_type != GL_TEXTURE as i32 {
                continue;
            }
            let candidate = DrawCandidate {
                texture: texture as u32,
                target: 0,
                layer,
                width: bounds.width,
                height: bounds.height,
                bounds,
                score: raster_area,
            };
            upsert_render_target(candidate);
            record_dependencies(candidate.key(), &sampled_names);
        }
        return;
    }
    record_presented(context as usize, &sampled_names, false);
    let best = sampled
        .iter()
        .max_by_key(|candidate| candidate.score)
        .copied();
    if let Some(candidate) = best {
        let mut candidates = draw_candidates()
            .lock()
            .unwrap_or_else(|error| error.into_inner());
        let candidates = candidates.entry(context as usize).or_default();
        if let Some(existing) = candidates.iter_mut().find(|existing| {
            (existing.texture, existing.target, existing.layer)
                == (candidate.texture, candidate.target, candidate.layer)
        }) {
            if candidate.score > existing.score {
                *existing = candidate;
            }
        } else {
            candidates.push(candidate);
        }
    }
}

fn framebuffer_attachment_query() -> Option<GlGetFramebufferAttachmentParameteriv> {
    if let Some(function) = GET_FRAMEBUFFER_ATTACHMENT.get() {
        return Some(*function);
    }
    let function: GlGetFramebufferAttachmentParameteriv =
        unsafe { proc(b"glGetFramebufferAttachmentParameteriv\0") }?;
    let _ = GET_FRAMEBUFFER_ATTACHMENT.set(function);
    Some(function)
}

type GlGetTextureLevelParameteriv = unsafe extern "system" fn(Gluint, i32, Glenum, *mut i32);
static GET_TEXTURE_LEVEL_PARAMETER: OnceLock<Option<GlGetTextureLevelParameteriv>> =
    OnceLock::new();

/// Queries a texture's level-zero extent and remembers it. Used for textures
/// first seen as blit or copy attachments, which are never bound to a
/// sampling unit where the draw observer would measure them. Prefers the
/// direct-state-access query, which handles any texture target and leaves the
/// application's bindings untouched; falls back to a temporary 2D binding.
unsafe fn measure_texture(texture: Gluint, target: Glenum) -> Option<(u32, u32)> {
    if let Some(&(_, width, height)) = texture_info()
        .lock()
        .unwrap_or_else(|error| error.into_inner())
        .get(&texture)
    {
        return Some((width, height));
    }
    let direct = *GET_TEXTURE_LEVEL_PARAMETER
        .get_or_init(|| unsafe { proc(b"glGetTextureLevelParameteriv\0") });
    let mut width = 0;
    let mut height = 0;
    if let Some(query) = direct {
        unsafe {
            query(texture, 0, GL_TEXTURE_WIDTH, &mut width);
            query(texture, 0, GL_TEXTURE_HEIGHT, &mut height);
        }
    } else if target == GL_TEXTURE_2D {
        let mut old_texture = 0;
        unsafe {
            glGetIntegerv(GL_TEXTURE_BINDING_2D, &mut old_texture);
            glBindTexture(GL_TEXTURE_2D, texture);
            glGetTexLevelParameteriv(GL_TEXTURE_2D, 0, GL_TEXTURE_WIDTH, &mut width);
            glGetTexLevelParameteriv(GL_TEXTURE_2D, 0, GL_TEXTURE_HEIGHT, &mut height);
            glBindTexture(GL_TEXTURE_2D, old_texture as u32);
        }
    } else {
        return None;
    }
    if width <= 1 || height <= 1 {
        return None;
    }
    texture_info()
        .lock()
        .unwrap_or_else(|error| error.into_inner())
        .insert(texture, (target, width as u32, height as u32));
    Some((width as u32, height as u32))
}

/// Follows texture-to-texture copies through the tracking graph. Copies are
/// how display-transfer emulation moves a finished frame into the surface the
/// presenter samples, so the destination is a render target fed by the source.
pub unsafe fn record_copy_candidate(
    src_name: Gluint,
    src_target: Glenum,
    dst_name: Gluint,
    dst_target: Glenum,
    dst_layer: i32,
    width: i32,
    height: i32,
) {
    if tracked_render_target_count().is_none() || !graph_tracking_enabled() {
        return;
    }
    if src_name == 0 || dst_name == 0 || width <= 1 || height <= 1 {
        return;
    }
    let context = unsafe { current_context() };
    if context.is_null() {
        return;
    }
    let inputs: Vec<Gluint> = if src_target == GL_RENDERBUFFER {
        renderbuffer_inputs()
            .lock()
            .unwrap_or_else(|error| error.into_inner())
            .get(&src_name)
            .map(|inputs| inputs.iter().map(|(texture, _)| *texture).collect())
            .unwrap_or_default()
    } else {
        vec![src_name]
    };
    if inputs.is_empty() {
        return;
    }
    if dst_target == GL_RENDERBUFFER {
        record_renderbuffer_inputs(dst_name, &inputs);
        return;
    }
    let Some((texture_width, texture_height)) = (unsafe { measure_texture(dst_name, dst_target) })
    else {
        return;
    };
    let candidate = DrawCandidate {
        texture: dst_name,
        target: 0,
        layer: if dst_target == GL_TEXTURE_2D_ARRAY {
            dst_layer
        } else {
            0
        },
        width: texture_width,
        height: texture_height,
        bounds: QuadBounds {
            x: 0,
            y: 0,
            width: texture_width,
            height: texture_height,
        },
        score: u64::from(texture_width) * u64::from(texture_height),
    };
    upsert_render_target(candidate);
    record_dependencies(candidate.key(), &inputs);
}

/// Follows framebuffer blits through the tracking graph. Blits into an
/// offscreen framebuffer make the destination a render target fed by the
/// source (compositors and display-transfer emulation copy screens this way),
/// and a blit into the default framebuffer is a presentation root, since no
/// default-framebuffer draw exists to observe.
pub unsafe fn record_blit_candidate() {
    if tracked_render_target_count().is_none() || !graph_tracking_enabled() {
        return;
    }
    let context = unsafe { current_context() };
    if context.is_null() {
        return;
    }
    let mut read_framebuffer = 0;
    let mut draw_framebuffer = 0;
    unsafe {
        glGetIntegerv(GL_READ_FRAMEBUFFER_BINDING, &mut read_framebuffer);
        glGetIntegerv(GL_DRAW_FRAMEBUFFER_BINDING, &mut draw_framebuffer);
    }
    if read_framebuffer == 0 {
        return;
    }
    let Some(function) = framebuffer_attachment_query() else {
        return;
    };
    let attachment = |target| {
        let mut object_type = 0;
        let mut object = 0;
        let mut layer = 0;
        unsafe {
            function(
                target,
                crate::interop::GL_COLOR_ATTACHMENT0,
                GL_FRAMEBUFFER_ATTACHMENT_OBJECT_TYPE,
                &mut object_type,
            );
            function(
                target,
                crate::interop::GL_COLOR_ATTACHMENT0,
                GL_FRAMEBUFFER_ATTACHMENT_OBJECT_NAME,
                &mut object,
            );
            function(
                target,
                crate::interop::GL_COLOR_ATTACHMENT0,
                GL_FRAMEBUFFER_ATTACHMENT_TEXTURE_LAYER,
                &mut layer,
            );
        }
        (object_type, object, layer)
    };
    let (source_type, source_object, _) = attachment(GL_READ_FRAMEBUFFER);
    if source_object <= 0 {
        return;
    }
    let inputs: Vec<Gluint> = if source_type == GL_RENDERBUFFER as i32 {
        // Re-presenting a renderbuffer keeps its recorded inputs alive even
        // when the scene is static and nothing redraws them.
        let now = generation();
        let mut all_inputs = renderbuffer_inputs()
            .lock()
            .unwrap_or_else(|error| error.into_inner());
        all_inputs
            .get_mut(&(source_object as u32))
            .map(|inputs| {
                inputs
                    .iter_mut()
                    .map(|(texture, seen)| {
                        *seen = now;
                        *texture
                    })
                    .collect()
            })
            .unwrap_or_default()
    } else if source_type == GL_TEXTURE as i32 {
        let texture = source_object as u32;
        if unsafe { measure_texture(texture, GL_TEXTURE_2D) }.is_some() {
            vec![texture]
        } else {
            Vec::new()
        }
    } else {
        Vec::new()
    };
    if inputs.is_empty() {
        return;
    }
    if draw_framebuffer == 0 {
        record_presented(context as usize, &inputs, true);
        return;
    }
    let (destination_type, destination_object, destination_layer) = attachment(GL_DRAW_FRAMEBUFFER);
    if destination_object <= 0 {
        return;
    }
    if destination_type == GL_RENDERBUFFER as i32 {
        record_renderbuffer_inputs(destination_object as u32, &inputs);
        return;
    }
    if destination_type != GL_TEXTURE as i32 {
        return;
    }
    let texture = destination_object as u32;
    let Some((width, height)) = (unsafe { measure_texture(texture, GL_TEXTURE_2D) }) else {
        return;
    };
    let candidate = DrawCandidate {
        texture,
        target: 0,
        layer: destination_layer,
        width,
        height,
        bounds: QuadBounds {
            x: 0,
            y: 0,
            width,
            height,
        },
        score: u64::from(width) * u64::from(height),
    };
    upsert_render_target(candidate);
    record_dependencies(candidate.key(), &inputs);
}

#[cfg(test)]
mod tests {
    use super::{
        DrawCandidate, QuadBounds, TargetKey, collapse_flip_pairs, intersect,
        select_from_dependency_graph, select_screen_cohort,
    };
    use std::collections::HashMap;

    fn candidate(texture: u32, width: u32, height: u32, score: u64) -> DrawCandidate {
        DrawCandidate {
            texture,
            target: 0x0de1,
            layer: 0,
            width,
            height,
            bounds: QuadBounds {
                x: 0,
                y: 0,
                width: 256,
                height: 384,
            },
            score,
        }
    }

    #[test]
    fn scissor_is_intersected_with_viewport() {
        assert_eq!(
            intersect([100, 50, 800, 600], [0, 120, 1920, 300]),
            Some([100, 120, 800, 300])
        );
        assert_eq!(intersect([0, 0, 100, 100], [200, 200, 10, 10]), None);
    }

    #[test]
    fn screen_cohort_keeps_distinct_same_sized_targets_in_draw_order() {
        let selected = select_screen_cohort(
            vec![
                candidate(10, 512, 384, 100),
                candidate(10, 512, 384, 100),
                candidate(11, 512, 384, 100),
                candidate(99, 32, 32, 10_000),
            ],
            2,
        );
        assert_eq!(
            selected
                .iter()
                .map(|candidate| candidate.texture)
                .collect::<Vec<_>>(),
            vec![10, 11]
        );
    }

    #[test]
    fn single_candidate_prefers_the_strongest_extent() {
        let selected = select_screen_cohort(
            vec![candidate(1, 384, 384, 200), candidate(2, 256, 192, 100)],
            2,
        );
        assert_eq!(selected.len(), 1);
        assert_eq!(selected[0].texture, 1);
    }

    #[test]
    fn screen_cohort_honors_the_configured_limit() {
        let selected = select_screen_cohort(
            vec![candidate(1, 256, 192, 100), candidate(2, 256, 192, 100)],
            1,
        );
        assert_eq!(selected.len(), 1);
        assert_eq!(selected[0].texture, 1);
    }

    #[test]
    fn dependency_graph_expands_a_compositor_into_independent_inputs() {
        let candidates = vec![
            candidate(30, 256, 192, 300),
            candidate(10, 256, 192, 100),
            candidate(20, 256, 192, 100),
        ];
        let dependencies = HashMap::from([(
            TargetKey {
                texture: 30,
                layer: 0,
            },
            vec![(10, 0), (20, 0)],
        )]);
        let selected = select_from_dependency_graph(&candidates, &dependencies, &[30], 2);
        assert_eq!(
            selected
                .iter()
                .map(|candidate| candidate.texture)
                .collect::<Vec<_>>(),
            vec![10, 20]
        );
    }

    #[test]
    fn dependency_graph_does_not_count_a_directly_sampled_input_twice() {
        let candidates = vec![
            candidate(30, 256, 192, 300),
            candidate(10, 256, 192, 100),
            candidate(20, 256, 192, 100),
        ];
        let dependencies = HashMap::from([(
            TargetKey {
                texture: 30,
                layer: 0,
            },
            vec![(10, 0), (20, 0)],
        )]);
        let selected = select_from_dependency_graph(&candidates, &dependencies, &[30, 10], 2);
        assert_eq!(
            selected
                .iter()
                .map(|candidate| candidate.texture)
                .collect::<Vec<_>>(),
            vec![10, 20]
        );
    }

    #[test]
    fn dependency_graph_keeps_independent_screens_of_different_sizes() {
        let candidates = vec![candidate(11, 240, 320, 100), candidate(19, 180, 400, 100)];
        let selected = select_from_dependency_graph(&candidates, &HashMap::new(), &[11, 19], 2);
        assert_eq!(
            selected
                .iter()
                .map(|candidate| (candidate.texture, candidate.width, candidate.height))
                .collect::<Vec<_>>(),
            vec![(11, 240, 320), (19, 180, 400)]
        );
    }

    #[test]
    fn dependency_graph_does_not_descend_copy_chains() {
        // A presented screen fed 1:1 from the buffer that produced it must be
        // captured itself rather than replaced by its producer.
        let candidates = vec![candidate(31, 240, 320, 100), candidate(5, 240, 320, 100)];
        let dependencies = HashMap::from([(
            TargetKey {
                texture: 31,
                layer: 0,
            },
            vec![(5, 0)],
        )]);
        let selected = select_from_dependency_graph(&candidates, &dependencies, &[31], 2);
        assert_eq!(
            selected
                .iter()
                .map(|candidate| candidate.texture)
                .collect::<Vec<_>>(),
            vec![31]
        );
    }

    #[test]
    fn flip_pairs_collapse_to_the_freshest_face() {
        // Page-flipped guest framebuffers share their source; distinct
        // screens do not and must both survive.
        let candidates = vec![
            candidate(23, 240, 320, 100),
            candidate(31, 240, 320, 100),
            candidate(19, 240, 400, 100),
        ];
        let dependencies = HashMap::from([
            (
                TargetKey {
                    texture: 23,
                    layer: 0,
                },
                vec![(5, 7)],
            ),
            (
                TargetKey {
                    texture: 31,
                    layer: 0,
                },
                vec![(5, 9)],
            ),
            (
                TargetKey {
                    texture: 19,
                    layer: 0,
                },
                vec![(6, 9)],
            ),
        ]);
        let mut roots = vec![23, 31, 19];
        collapse_flip_pairs(&mut roots, &candidates, &dependencies);
        assert_eq!(roots, vec![31, 19]);
    }

    #[test]
    fn dependency_graph_treats_mrt_array_layers_as_distinct_targets() {
        let mut layer_zero = candidate(40, 512, 384, 100);
        layer_zero.target = super::GL_TEXTURE_2D_ARRAY;
        let mut layer_one = layer_zero;
        layer_one.layer = 1;
        let selected =
            select_from_dependency_graph(&[layer_zero, layer_one], &HashMap::new(), &[40], 2);
        assert_eq!(
            selected
                .iter()
                .map(|candidate| candidate.layer)
                .collect::<Vec<_>>(),
            vec![0, 1]
        );
    }
}
