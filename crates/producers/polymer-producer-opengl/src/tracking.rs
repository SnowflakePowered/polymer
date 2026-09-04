//! Heuristics for finding the game texture and its presented bounds.

use crate::interop::{
    GL_ACTIVE_TEXTURE, GL_DRAW_FRAMEBUFFER, GL_DRAW_FRAMEBUFFER_BINDING, GL_READ_FRAMEBUFFER,
    GL_READ_FRAMEBUFFER_BINDING, GL_SCISSOR_BOX, GL_SCISSOR_TEST, GL_TEXTURE_2D,
    GL_TEXTURE_2D_ARRAY, GL_TEXTURE_BINDING_2D, GL_TEXTURE_BINDING_2D_ARRAY, GL_TEXTURE_HEIGHT,
    GL_TEXTURE_WIDTH, GL_TEXTURE0, GL_VIEWPORT, GlActiveTexture, Glenum, Gluint, current_context,
    glBindTexture, glGetIntegerv, glGetTexLevelParameteriv, glIsEnabled, proc,
};
use parking_lot::Mutex;
use std::{
    collections::{HashMap, HashSet},
    ffi::c_void,
    sync::{
        OnceLock,
        atomic::{AtomicU64, Ordering},
    },
};

const STALE_HORIZON: u64 = 8;

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
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
    /// Whether `bounds` is the raster area of a draw into the default
    /// framebuffer. Only such bounds describe where the content lands in the
    /// window; bounds observed on offscreen draws are in that framebuffer's
    /// own coordinate space and must not be used as presentation placement.
    pub(crate) window_bounds: bool,
    pub(crate) score: u64,
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

type GlGetFramebufferAttachmentParameteriv =
    unsafe extern "system" fn(Glenum, Glenum, Glenum, *mut i32);
type GlGetTextureLevelParameteriv = unsafe extern "system" fn(Gluint, i32, Glenum, *mut i32);

/// Function pointers used to read a present draw's quad geometry and its
/// transform uniforms back from the pipeline.
#[derive(Clone, Copy)]
struct QuadReader {
    get_vertex_attrib_iv: unsafe extern "system" fn(Gluint, Glenum, *mut i32),
    get_vertex_attrib_pointer_v: unsafe extern "system" fn(Gluint, Glenum, *mut *mut c_void),
    get_buffer_sub_data: unsafe extern "system" fn(Glenum, isize, isize, *mut c_void),
    bind_buffer: unsafe extern "system" fn(Glenum, Gluint),
    get_program_iv: unsafe extern "system" fn(Gluint, Glenum, *mut i32),
    get_active_uniform:
        unsafe extern "system" fn(Gluint, Gluint, i32, *mut i32, *mut i32, *mut Glenum, *mut u8),
    get_uniform_location: unsafe extern "system" fn(Gluint, *const u8) -> i32,
    get_uniform_fv: unsafe extern "system" fn(Gluint, i32, *mut f32),
}

/// Process-wide state shared by the installed OpenGL hook contexts.
#[derive(Default)]
pub struct Tracking {
    generation: AtomicU64,
    draw_candidates: Mutex<HashMap<usize, Vec<DrawCandidate>>>,
    render_targets: Mutex<Vec<(DrawCandidate, u64)>>,
    texture_info: Mutex<HashMap<Gluint, (Glenum, u32, u32)>>,
    dependencies: Mutex<HashMap<TargetKey, Vec<(Gluint, u64)>>>,
    presented_textures: Mutex<HashMap<usize, Vec<(Gluint, bool)>>>,
    /// Present-draw quads observed this frame, in draw order: the sampled
    /// texture and where its quad lands in the window (bottom-up).
    presented_quads: Mutex<HashMap<usize, Vec<(Gluint, QuadBounds)>>>,
    renderbuffer_inputs: Mutex<HashMap<Gluint, Vec<(Gluint, u64)>>>,
    active_texture: OnceLock<GlActiveTexture>,
    get_framebuffer_attachment: OnceLock<GlGetFramebufferAttachmentParameteriv>,
    get_texture_level_parameter: OnceLock<Option<GlGetTextureLevelParameteriv>>,
    quad_reader: OnceLock<Option<QuadReader>>,
    quad_logs: AtomicU64,
}

const GL_FRAMEBUFFER_ATTACHMENT_OBJECT_TYPE: Glenum = 0x8CD0;
const GL_FRAMEBUFFER_ATTACHMENT_OBJECT_NAME: Glenum = 0x8CD1;
const GL_FRAMEBUFFER_ATTACHMENT_TEXTURE_LAYER: Glenum = 0x8CD4;
const GL_TEXTURE: Glenum = 0x1702;
const GL_RENDERBUFFER: Glenum = 0x8D41;
const GL_MAX_DRAW_BUFFERS: Glenum = 0x8824;
const GL_DRAW_BUFFER0: Glenum = 0x8825;
const GL_FLOAT: Glenum = 0x1406;
const GL_ARRAY_BUFFER: Glenum = 0x8892;
const GL_ARRAY_BUFFER_BINDING: Glenum = 0x8894;
const GL_CURRENT_PROGRAM: Glenum = 0x8B8D;
const GL_ACTIVE_UNIFORMS: Glenum = 0x8B86;
const GL_FLOAT_VEC2: Glenum = 0x8B50;
const GL_FLOAT_MAT2X3: Glenum = 0x8B65;
const GL_VERTEX_ATTRIB_ARRAY_ENABLED: Glenum = 0x8622;
const GL_VERTEX_ATTRIB_ARRAY_SIZE: Glenum = 0x8623;
const GL_VERTEX_ATTRIB_ARRAY_STRIDE: Glenum = 0x8624;
const GL_VERTEX_ATTRIB_ARRAY_TYPE: Glenum = 0x8625;
const GL_VERTEX_ATTRIB_ARRAY_BUFFER_BINDING: Glenum = 0x889F;
const GL_VERTEX_ATTRIB_ARRAY_POINTER: Glenum = 0x8645;

impl Tracking {
    fn generation(&self) -> u64 {
        self.generation.load(Ordering::Relaxed)
    }

    fn upsert_render_target(&self, candidate: DrawCandidate) {
        let now = self.generation();
        let mut targets = self.render_targets.lock();
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

    fn record_dependencies(&self, key: TargetKey, sources: &[Gluint]) {
        let now = self.generation();
        let mut graphs = self.dependencies.lock();
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

    fn record_renderbuffer_inputs(&self, renderbuffer: Gluint, sources: &[Gluint]) {
        let now = self.generation();
        let mut all_inputs = self.renderbuffer_inputs.lock();
        let inputs = all_inputs.entry(renderbuffer).or_default();
        for &source in sources {
            if let Some((_, seen)) = inputs.iter_mut().find(|(texture, _)| *texture == source) {
                *seen = now;
            } else {
                inputs.push((source, now));
            }
        }
    }

    fn record_presented(&self, context: usize, sources: &[Gluint], via_blit: bool) {
        {
            let mut presented = self.presented_textures.lock();
            let presented = presented.entry(context).or_default();
            for &source in sources {
                if let Some((_, blit)) =
                    presented.iter_mut().find(|(texture, _)| *texture == source)
                {
                    *blit |= via_blit;
                } else {
                    presented.push((source, via_blit));
                }
            }
        }
        self.touch_render_targets(sources);
    }

    /// Refreshes the staleness stamps of render targets that are still reaching
    /// the screen. A static scene re-presents buffers without redrawing them, and
    /// their history must not decay while that lasts.
    fn touch_render_targets(&self, sources: &[Gluint]) {
        let now = self.generation();
        let mut targets = self.render_targets.lock();
        for (candidate, seen) in targets.iter_mut() {
            if sources.contains(&candidate.texture) {
                *seen = now;
            }
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
impl Tracking {
    pub(crate) fn take_draw_candidates(&self, context: usize) -> (Vec<DrawCandidate>, bool) {
        let Some(limit) = polymer_hook::config::tracked_render_target_count() else {
            return (Vec::new(), false);
        };
        let now = self.generation.fetch_add(1, Ordering::Relaxed) + 1;
        let horizon = now.saturating_sub(STALE_HORIZON);
        let fallback = self
            .draw_candidates
            .lock()
            .remove(&context)
            .unwrap_or_default();
        let presented = self
            .presented_textures
            .lock()
            .remove(&context)
            .unwrap_or_default();
        let mut targets = {
            let mut store = self.render_targets.lock();
            store.retain(|(_, seen)| *seen >= horizon);
            store
                .iter()
                .map(|(candidate, _)| *candidate)
                .collect::<Vec<_>>()
        };
        {
            let info = self.texture_info.lock();
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
            let mut graphs = self.dependencies.lock();
            graphs.retain(|_, inputs| {
                inputs.retain(|(_, seen)| *seen >= horizon);
                !inputs.is_empty()
            });
            graphs.clone()
        };
        self.renderbuffer_inputs.lock().retain(|_, inputs| {
            inputs.retain(|(_, seen)| *seen >= horizon);
            !inputs.is_empty()
        });
        if !polymer_hook::config::graph_tracking_enabled() {
            return (
                select_screen_cohort(
                    if fallback.is_empty() {
                        targets
                    } else {
                        fallback
                    },
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
            let info = self.texture_info.lock();
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
                    window_bounds: false,
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
                candidate.window_bounds = false;
            }
            return (graph_selected, true);
        }
        if !graph_selected.is_empty() {
            return (select_screen_cohort(graph_selected, limit), false);
        }
        (
            select_screen_cohort(
                if fallback.is_empty() {
                    targets
                } else {
                    fallback
                },
                limit,
            ),
            false,
        )
    }

    /// Takes the present-draw quads observed since the last swap: the sampled
    /// texture and where its quad lands in the window (bottom-up), in draw
    /// order. The capture layer pairs these with the final selection.
    pub(crate) fn take_present_quads(&self, context: usize) -> Vec<(Gluint, QuadBounds)> {
        self.presented_quads
            .lock()
            .remove(&context)
            .unwrap_or_default()
    }

    /// Clears the accumulated dependency graph and render-target history so the
    /// next frames rebuild it from scratch. A live renderer-scale switch
    /// reallocates the screen textures mid-stream; the incremental selection
    /// otherwise carries stale edges from the old resolution and re-resolves
    /// only one of the two screens, where a from-empty rebuild (exactly what a
    /// fresh launch does) recovers both. `texture_info` is kept — it is
    /// re-measured on demand and holds no cross-frame selection state.
    pub(crate) fn reset_graph(&self) {
        self.render_targets.lock().clear();
        self.dependencies.lock().clear();
        self.renderbuffer_inputs.lock().clear();
        self.presented_textures.lock().clear();
        self.draw_candidates.lock().clear();
    }

    /// Re-validates a held selection against the live GPU texture state,
    /// dropping slots whose texture was reallocated to a different extent or
    /// deleted — a renderer scale switch does both. This re-measures each
    /// texture rather than trusting the cached `texture_info`, which is not
    /// refreshed for a screen the application stopped binding. Without it a
    /// resolution switch leaves the capture blitting a stale-extent region
    /// from a texture that now holds differently sized content (the reported
    /// mismatch), and the stale slot blocks the new-extent selection.
    ///
    /// Called on the swap thread with the presenting context current, so the
    /// direct-state-access queries are safe. Sources whose live extent still
    /// matches are kept; those whose texture cannot be measured (the query is
    /// unavailable) are kept as well, so a driver without DSA degrades to the
    /// prior behavior rather than dropping everything.
    pub(crate) fn refresh_sources(&self, sources: &[DrawCandidate]) -> Vec<DrawCandidate> {
        if sources.is_empty() {
            return Vec::new();
        }
        let Some(query) = *self
            .get_texture_level_parameter
            .get_or_init(|| unsafe { proc(b"glGetTextureLevelParameteriv\0") })
        else {
            return sources.to_vec();
        };
        let mut info = self.texture_info.lock();
        sources
            .iter()
            .filter(|source| {
                let mut width = 0;
                let mut height = 0;
                unsafe {
                    query(source.texture, 0, GL_TEXTURE_WIDTH, &mut width);
                    query(source.texture, 0, GL_TEXTURE_HEIGHT, &mut height);
                }
                // A deleted texture answers 0; a live one answers its current
                // extent, which the cache is updated to match so downstream
                // measurements and the merge agree on the reallocated size.
                if width <= 1 || height <= 1 {
                    info.remove(&source.texture);
                    return false;
                }
                if let Some(entry) = info.get_mut(&source.texture) {
                    entry.1 = width as u32;
                    entry.2 = height as u32;
                }
                (width as u32, height as u32) == (source.width, source.height)
            })
            .copied()
            .collect()
    }
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
impl Tracking {
    pub unsafe fn record_draw_candidate(&self, arrays_range: Option<(i32, i32)>) {
        if polymer_hook::config::tracked_render_target_count().is_none() {
            return;
        }
        let context = unsafe { current_context() };
        if context.is_null() {
            return;
        }
        let mut draw_framebuffer = 0;
        unsafe { glGetIntegerv(GL_DRAW_FRAMEBUFFER_BINDING, &mut draw_framebuffer) };
        let active_texture = if let Some(function) = self.active_texture.get() {
            *function
        } else {
            let Some(function): Option<GlActiveTexture> = (unsafe { proc(b"glActiveTexture\0") })
            else {
                return;
            };
            let _ = self.active_texture.set(function);
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
        // A present draw's raster area is usually the whole window; the quad
        // it actually draws is the real placement (screen gaps, layouts). It
        // is kept out of the candidate bounds — placement pairing happens
        // against the final selection, per present, in the capture layer.
        let mut present_quad = None;
        if draw_framebuffer == 0
            && let Some((first, count)) = arrays_range
            && let Some(quad) = unsafe { self.present_quad_bounds(first, count, viewport) }
        {
            let logs = self.quad_logs.fetch_add(1, Ordering::Relaxed);
            if logs < 32 || logs.is_multiple_of(600) {
                tracing::debug!(
                    "polymer: present quad first={first} count={count} -> {},{} {}x{} (bottom-up, viewport {}x{})",
                    quad.x,
                    quad.y,
                    quad.width,
                    quad.height,
                    viewport[2],
                    viewport[3]
                );
            }
            present_quad = Some(quad);
        }
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
                    window_bounds: draw_framebuffer == 0,
                    score: raster_area
                        .saturating_mul(width as u64 * height as u64)
                        .saturating_mul(aspect_match),
                });
                self.texture_info
                    .lock()
                    .insert(texture as u32, (target, width as u32, height as u32));
            }
        }
        unsafe { active_texture(old_active as u32) };
        let sampled_names = sampled
            .iter()
            .map(|candidate| candidate.texture)
            .collect::<Vec<_>>();
        if draw_framebuffer != 0 {
            let Some(function) = self.framebuffer_attachment_query() else {
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
                    self.record_renderbuffer_inputs(texture as u32, &sampled_names);
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
                    window_bounds: false,
                    score: raster_area,
                };
                self.upsert_render_target(candidate);
                self.record_dependencies(candidate.key(), &sampled_names);
            }
            return;
        }
        self.record_presented(context as usize, &sampled_names, false);
        let best = sampled
            .iter()
            .max_by_key(|candidate| candidate.score)
            .copied();
        if let (Some(quad), Some(candidate)) = (present_quad, best) {
            self.presented_quads
                .lock()
                .entry(context as usize)
                .or_default()
                .push((candidate.texture, quad));
        }
        if let Some(candidate) = best {
            let mut candidates = self.draw_candidates.lock();
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

    fn quad_reader(&self) -> Option<QuadReader> {
        *self.quad_reader.get_or_init(|| {
            Some(QuadReader {
                get_vertex_attrib_iv: unsafe { proc(b"glGetVertexAttribiv\0") }?,
                get_vertex_attrib_pointer_v: unsafe { proc(b"glGetVertexAttribPointerv\0") }?,
                get_buffer_sub_data: unsafe { proc(b"glGetBufferSubData\0") }?,
                bind_buffer: unsafe { proc(b"glBindBuffer\0") }?,
                get_program_iv: unsafe { proc(b"glGetProgramiv\0") }?,
                get_active_uniform: unsafe { proc(b"glGetActiveUniform\0") }?,
                get_uniform_location: unsafe { proc(b"glGetUniformLocation\0") }?,
                get_uniform_fv: unsafe { proc(b"glGetUniformfv\0") }?,
            })
        })
    }

    /// Reads a present draw's vertex positions back from the bound vertex
    /// buffer and evaluates the common 2D presentation pattern: screen-local
    /// quads transformed by a single `mat2x3` uniform into a layout space
    /// whose extent is a single `vec2` uniform (the melonDS present shader,
    /// but written by many Qt-based frontends). Returns the quad's placement
    /// in bottom-up window coordinates, matching raster-derived bounds. Any
    /// draw that does not match the pattern resolves to None and keeps the
    /// conservative raster bounds.
    unsafe fn present_quad_bounds(
        &self,
        first: i32,
        count: i32,
        viewport: [i32; 4],
    ) -> Option<QuadBounds> {
        if !(3..=8).contains(&count) || first < 0 || viewport[2] <= 0 || viewport[3] <= 0 {
            return None;
        }
        let reader = self.quad_reader()?;
        let mut program = 0;
        unsafe { glGetIntegerv(GL_CURRENT_PROGRAM, &mut program) };
        if program <= 0 {
            return None;
        }
        let program = program as u32;
        // Attribute 0 must be an enabled, buffer-backed vec2 float array —
        // the position layout of a 2D screen quad.
        let attrib = |name: Glenum| {
            let mut value = 0;
            unsafe { (reader.get_vertex_attrib_iv)(0, name, &mut value) };
            value
        };
        if attrib(GL_VERTEX_ATTRIB_ARRAY_ENABLED) == 0
            || attrib(GL_VERTEX_ATTRIB_ARRAY_SIZE) != 2
            || attrib(GL_VERTEX_ATTRIB_ARRAY_TYPE) != GL_FLOAT as i32
        {
            return None;
        }
        let buffer = attrib(GL_VERTEX_ATTRIB_ARRAY_BUFFER_BINDING);
        if buffer <= 0 {
            return None;
        }
        let stride = attrib(GL_VERTEX_ATTRIB_ARRAY_STRIDE);
        let stride = if stride <= 0 { 8 } else { stride as usize };
        let mut pointer = std::ptr::null_mut();
        unsafe {
            (reader.get_vertex_attrib_pointer_v)(0, GL_VERTEX_ATTRIB_ARRAY_POINTER, &mut pointer);
        }
        let offset = (pointer as usize).checked_add(first as usize * stride)?;
        let mut data = vec![0u8; (count as usize - 1) * stride + 8];
        let mut old_buffer = 0;
        unsafe {
            glGetIntegerv(GL_ARRAY_BUFFER_BINDING, &mut old_buffer);
            (reader.bind_buffer)(GL_ARRAY_BUFFER, buffer as u32);
            (reader.get_buffer_sub_data)(
                GL_ARRAY_BUFFER,
                offset as isize,
                data.len() as isize,
                data.as_mut_ptr().cast(),
            );
            (reader.bind_buffer)(GL_ARRAY_BUFFER, old_buffer as u32);
        }
        let mut low = (f32::MAX, f32::MAX);
        let mut high = (f32::MIN, f32::MIN);
        for vertex in 0..count as usize {
            let at = vertex * stride;
            let x = f32::from_le_bytes(data.get(at..at + 4)?.try_into().ok()?);
            let y = f32::from_le_bytes(data.get(at + 4..at + 8)?.try_into().ok()?);
            if !x.is_finite() || !y.is_finite() {
                return None;
            }
            low = (low.0.min(x), low.1.min(y));
            high = (high.0.max(x), high.1.max(y));
        }
        // The pattern requires exactly one mat2x3 transform and one vec2
        // layout extent among the program's uniforms; a second of either
        // kind makes the interpretation ambiguous.
        let mut uniform_count = 0;
        unsafe { (reader.get_program_iv)(program, GL_ACTIVE_UNIFORMS, &mut uniform_count) };
        let mut transform = None;
        let mut layout_extent = None;
        for index in 0..uniform_count.max(0) as u32 {
            let mut name = [0u8; 128];
            let (mut length, mut size, mut kind) = (0, 0, 0);
            unsafe {
                (reader.get_active_uniform)(
                    program,
                    index,
                    name.len() as i32 - 1,
                    &mut length,
                    &mut size,
                    &mut kind,
                    name.as_mut_ptr(),
                );
            }
            if kind != GL_FLOAT_MAT2X3 && kind != GL_FLOAT_VEC2 {
                continue;
            }
            let location = unsafe { (reader.get_uniform_location)(program, name.as_ptr()) };
            if location < 0 {
                continue;
            }
            if kind == GL_FLOAT_MAT2X3 {
                if transform.is_some() {
                    return None;
                }
                let mut values = [0f32; 6];
                unsafe { (reader.get_uniform_fv)(program, location, values.as_mut_ptr()) };
                transform = Some(values);
            } else {
                if layout_extent.is_some() {
                    return None;
                }
                let mut values = [0f32; 2];
                unsafe { (reader.get_uniform_fv)(program, location, values.as_mut_ptr()) };
                layout_extent = Some(values);
            }
        }
        let transform = transform?;
        let [layout_width, layout_height] = layout_extent?;
        if !(layout_width > 0.0 && layout_height > 0.0) {
            return None;
        }
        // `vec3(position, 1) * mat2x3`: each output component dots the input
        // with one stored column (the upload's transpose flag is already
        // resolved into the stored values).
        let map = |x: f32, y: f32| {
            (
                transform[0] * x + transform[1] * y + transform[2],
                transform[3] * x + transform[4] * y + transform[5],
            )
        };
        let corners = [
            map(low.0, low.1),
            map(high.0, low.1),
            map(low.0, high.1),
            map(high.0, high.1),
        ];
        let mut lo = (f32::MAX, f32::MAX);
        let mut hi = (f32::MIN, f32::MIN);
        for (x, y) in corners {
            lo = (lo.0.min(x), lo.1.min(y));
            hi = (hi.0.max(x), hi.1.max(y));
        }
        // Layout space is top-down (the shader flips y into GL's bottom-up
        // clip space): normalize by the layout extent, scale to the
        // viewport, and store bottom-up to match raster-derived bounds.
        let width = viewport[2] as f32;
        let height = viewport[3] as f32;
        let left = (lo.0 / layout_width * width).round().clamp(0.0, width) as u32;
        let top = (lo.1 / layout_height * height).round().clamp(0.0, height) as u32;
        let right = (hi.0 / layout_width * width).round().clamp(0.0, width) as u32;
        let bottom = (hi.1 / layout_height * height).round().clamp(0.0, height) as u32;
        if right <= left || bottom <= top {
            return None;
        }
        Some(QuadBounds {
            x: left,
            y: viewport[3] as u32 - bottom,
            width: right - left,
            height: bottom - top,
        })
    }

    fn framebuffer_attachment_query(&self) -> Option<GlGetFramebufferAttachmentParameteriv> {
        if let Some(function) = self.get_framebuffer_attachment.get() {
            return Some(*function);
        }
        let function: GlGetFramebufferAttachmentParameteriv =
            unsafe { proc(b"glGetFramebufferAttachmentParameteriv\0") }?;
        let _ = self.get_framebuffer_attachment.set(function);
        Some(function)
    }

    /// Queries a texture's level-zero extent and remembers it. Used for textures
    /// first seen as blit or copy attachments, which are never bound to a
    /// sampling unit where the draw observer would measure them. Prefers the
    /// direct-state-access query, which handles any texture target and leaves the
    /// application's bindings untouched; falls back to a temporary 2D binding.
    unsafe fn measure_texture(&self, texture: Gluint, target: Glenum) -> Option<(u32, u32)> {
        if let Some(&(_, width, height)) = self.texture_info.lock().get(&texture) {
            return Some((width, height));
        }
        let direct = *self
            .get_texture_level_parameter
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
        self.texture_info
            .lock()
            .insert(texture, (target, width as u32, height as u32));
        Some((width as u32, height as u32))
    }

    /// Follows texture-to-texture copies through the tracking graph. Copies are
    /// how display-transfer emulation moves a finished frame into the surface the
    /// presenter samples, so the destination is a render target fed by the source.
    pub unsafe fn record_copy_candidate(
        &self,
        src_name: Gluint,
        src_target: Glenum,
        dst_name: Gluint,
        dst_target: Glenum,
        dst_layer: i32,
        width: i32,
        height: i32,
    ) {
        if polymer_hook::config::tracked_render_target_count().is_none()
            || !polymer_hook::config::graph_tracking_enabled()
        {
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
            self.renderbuffer_inputs
                .lock()
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
            self.record_renderbuffer_inputs(dst_name, &inputs);
            return;
        }
        let Some((texture_width, texture_height)) =
            (unsafe { self.measure_texture(dst_name, dst_target) })
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
            window_bounds: false,
            score: u64::from(texture_width) * u64::from(texture_height),
        };
        self.upsert_render_target(candidate);
        self.record_dependencies(candidate.key(), &inputs);
    }

    /// Follows framebuffer blits through the tracking graph. Blits into an
    /// offscreen framebuffer make the destination a render target fed by the
    /// source (compositors and display-transfer emulation copy screens this way),
    /// and a blit into the default framebuffer is a presentation root, since no
    /// default-framebuffer draw exists to observe.
    pub unsafe fn record_blit_candidate(&self) {
        if polymer_hook::config::tracked_render_target_count().is_none()
            || !polymer_hook::config::graph_tracking_enabled()
        {
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
        let Some(function) = self.framebuffer_attachment_query() else {
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
            let now = self.generation();
            let mut all_inputs = self.renderbuffer_inputs.lock();
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
            if unsafe { self.measure_texture(texture, GL_TEXTURE_2D) }.is_some() {
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
            self.record_presented(context as usize, &inputs, true);
            return;
        }
        let (destination_type, destination_object, destination_layer) =
            attachment(GL_DRAW_FRAMEBUFFER);
        if destination_object <= 0 {
            return;
        }
        if destination_type == GL_RENDERBUFFER as i32 {
            self.record_renderbuffer_inputs(destination_object as u32, &inputs);
            return;
        }
        if destination_type != GL_TEXTURE as i32 {
            return;
        }
        let texture = destination_object as u32;
        let Some((width, height)) = (unsafe { self.measure_texture(texture, GL_TEXTURE_2D) })
        else {
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
            window_bounds: false,
            score: u64::from(width) * u64::from(height),
        };
        self.upsert_render_target(candidate);
        self.record_dependencies(candidate.key(), &inputs);
    }
}

#[cfg(test)]
mod tests {
    use super::{
        DrawCandidate, QuadBounds, TargetKey, Tracking, collapse_flip_pairs, intersect,
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
            window_bounds: false,
            score,
        }
    }

    #[test]
    fn refresh_without_a_measurement_query_is_a_no_op() {
        // No GL context in unit tests, so the direct-state-access query fails
        // to resolve; refresh must then keep the held selection verbatim
        // rather than drop everything.
        let tracking = Tracking::default();
        let sources = [candidate(11, 256, 192, 1), candidate(12, 256, 192, 1)];
        let refreshed = tracking.refresh_sources(&sources);
        assert_eq!(
            refreshed
                .iter()
                .map(|source| source.texture)
                .collect::<Vec<_>>(),
            vec![11, 12]
        );
    }

    #[test]
    fn tracking_instances_do_not_share_textures() {
        let first = Tracking::default();
        let second = Tracking::default();
        first.texture_info.lock().insert(7, (0x0de1, 640, 480));

        assert!(first.texture_info.lock().contains_key(&7));
        assert!(!second.texture_info.lock().contains_key(&7));
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
