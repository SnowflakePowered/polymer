//! Heuristics for finding the game texture and its presented bounds.

use crate::interop::{
    GL_ACTIVE_TEXTURE, GL_DRAW_FRAMEBUFFER, GL_DRAW_FRAMEBUFFER_BINDING, GL_SCISSOR_BOX,
    GL_SCISSOR_TEST, GL_TEXTURE_2D, GL_TEXTURE_2D_ARRAY, GL_TEXTURE_BINDING_2D,
    GL_TEXTURE_BINDING_2D_ARRAY, GL_TEXTURE_HEIGHT, GL_TEXTURE_WIDTH, GL_TEXTURE0, GL_VIEWPORT,
    GlActiveTexture, Glenum, Gluint, current_context, glGetIntegerv, glGetTexLevelParameteriv,
    glIsEnabled, proc,
};
use std::{
    collections::{HashMap, HashSet},
    sync::{Mutex, OnceLock},
};

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

static DRAW_CANDIDATES: OnceLock<Mutex<HashMap<usize, Vec<DrawCandidate>>>> = OnceLock::new();
static RENDER_TARGET_CANDIDATES: OnceLock<Mutex<HashMap<usize, Vec<DrawCandidate>>>> =
    OnceLock::new();
static TEXTURE_INFO: OnceLock<Mutex<HashMap<(usize, Gluint), (Glenum, u32, u32)>>> =
    OnceLock::new();
static DEPENDENCIES: OnceLock<Mutex<HashMap<usize, HashMap<TargetKey, Vec<Gluint>>>>> =
    OnceLock::new();
static PRESENTED_TEXTURES: OnceLock<Mutex<HashMap<usize, Vec<Gluint>>>> = OnceLock::new();
static ACTIVE_TEXTURE: OnceLock<GlActiveTexture> = OnceLock::new();
type GlGetFramebufferAttachmentParameteriv =
    unsafe extern "system" fn(Glenum, Glenum, Glenum, *mut i32);
static GET_FRAMEBUFFER_ATTACHMENT: OnceLock<GlGetFramebufferAttachmentParameteriv> =
    OnceLock::new();

const GL_FRAMEBUFFER_ATTACHMENT_OBJECT_TYPE: Glenum = 0x8CD0;
const GL_FRAMEBUFFER_ATTACHMENT_OBJECT_NAME: Glenum = 0x8CD1;
const GL_FRAMEBUFFER_ATTACHMENT_TEXTURE_LAYER: Glenum = 0x8CD4;
const GL_TEXTURE: Glenum = 0x1702;
const GL_MAX_DRAW_BUFFERS: Glenum = 0x8824;
const GL_DRAW_BUFFER0: Glenum = 0x8825;

fn draw_candidates() -> &'static Mutex<HashMap<usize, Vec<DrawCandidate>>> {
    DRAW_CANDIDATES.get_or_init(|| Mutex::new(HashMap::new()))
}

fn render_target_candidates() -> &'static Mutex<HashMap<usize, Vec<DrawCandidate>>> {
    RENDER_TARGET_CANDIDATES.get_or_init(|| Mutex::new(HashMap::new()))
}

fn texture_info() -> &'static Mutex<HashMap<(usize, Gluint), (Glenum, u32, u32)>> {
    TEXTURE_INFO.get_or_init(|| Mutex::new(HashMap::new()))
}

fn dependencies() -> &'static Mutex<HashMap<usize, HashMap<TargetKey, Vec<Gluint>>>> {
    DEPENDENCIES.get_or_init(|| Mutex::new(HashMap::new()))
}

fn presented_textures() -> &'static Mutex<HashMap<usize, Vec<Gluint>>> {
    PRESENTED_TEXTURES.get_or_init(|| Mutex::new(HashMap::new()))
}

fn intersect(a: [i32; 4], b: [i32; 4]) -> Option<[i32; 4]> {
    let left = a[0].max(b[0]);
    let bottom = a[1].max(b[1]);
    let right = a[0].saturating_add(a[2]).min(b[0].saturating_add(b[2]));
    let top = a[1].saturating_add(a[3]).min(b[1].saturating_add(b[3]));
    (right > left && top > bottom).then_some([left, bottom, right - left, top - bottom])
}

pub(crate) fn take_draw_candidates(context: usize) -> Vec<DrawCandidate> {
    let Some(limit) = tracked_render_target_count() else {
        return Vec::new();
    };
    let fallback = draw_candidates()
        .lock()
        .unwrap_or_else(|error| error.into_inner())
        .remove(&context)
        .unwrap_or_default();
    let mut targets = render_target_candidates()
        .lock()
        .unwrap_or_else(|error| error.into_inner())
        .remove(&context)
        .unwrap_or_default();
    let info = texture_info()
        .lock()
        .unwrap_or_else(|error| error.into_inner());
    targets.retain_mut(|candidate| {
        let Some(&(target, width, height)) = info.get(&(context, candidate.texture)) else {
            return false;
        };
        candidate.target = target;
        candidate.width = width;
        candidate.height = height;
        true
    });
    drop(info);
    let dependency_graph = dependencies()
        .lock()
        .unwrap_or_else(|error| error.into_inner())
        .remove(&context)
        .unwrap_or_default();
    let presented = presented_textures()
        .lock()
        .unwrap_or_else(|error| error.into_inner())
        .remove(&context)
        .unwrap_or_default();
    let graph_selected =
        select_from_dependency_graph(&targets, &dependency_graph, &presented, limit);
    select_screen_cohort(
        if !graph_selected.is_empty() {
            graph_selected
        } else if targets.is_empty() {
            fallback
        } else {
            targets
        },
        limit,
    )
}

fn select_from_dependency_graph(
    candidates: &[DrawCandidate],
    dependencies: &HashMap<TargetKey, Vec<Gluint>>,
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
                .flat_map(|texture| nodes_for_texture(*texture))
                .filter(|input| *input != *key)
                .collect::<Vec<_>>();
            (!inputs.is_empty()).then_some((index, inputs))
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
    dependencies: &HashMap<TargetKey, Vec<Gluint>>,
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
        if inputs.contains(&texture) {
            return true;
        }
        pending.extend(
            candidates
                .iter()
                .filter(|candidate| inputs.contains(&candidate.texture))
                .map(|candidate| candidate.key()),
        );
    }
    false
}

fn deduplicate(values: &mut Vec<TargetKey>) {
    let mut seen = HashSet::new();
    values.retain(|value| seen.insert(*value));
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
                .insert(
                    (context as usize, texture as u32),
                    (target, width as u32, height as u32),
                );
        }
    }
    unsafe { active_texture(old_active as u32) };
    if draw_framebuffer != 0 {
        let function = if let Some(function) = GET_FRAMEBUFFER_ATTACHMENT.get() {
            *function
        } else {
            let Some(function): Option<GlGetFramebufferAttachmentParameteriv> =
                (unsafe { proc(b"glGetFramebufferAttachmentParameteriv\0") })
            else {
                return;
            };
            let _ = GET_FRAMEBUFFER_ATTACHMENT.set(function);
            function
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
            if object_type != GL_TEXTURE as i32 || texture <= 0 {
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
            let mut targets = render_target_candidates()
                .lock()
                .unwrap_or_else(|error| error.into_inner());
            let targets = targets.entry(context as usize).or_default();
            if let Some(existing) = targets.iter_mut().find(|existing| {
                (existing.texture, existing.layer) == (candidate.texture, candidate.layer)
            }) {
                if candidate.score > existing.score {
                    *existing = candidate;
                }
            } else {
                targets.push(candidate);
            }
            let mut graphs = dependencies()
                .lock()
                .unwrap_or_else(|error| error.into_inner());
            let inputs = graphs
                .entry(context as usize)
                .or_default()
                .entry(candidate.key())
                .or_default();
            for source in &sampled {
                if source.texture != candidate.texture && !inputs.contains(&source.texture) {
                    inputs.push(source.texture);
                }
            }
        }
        return;
    }
    let best = sampled
        .iter()
        .max_by_key(|candidate| candidate.score)
        .copied();
    let mut presented = presented_textures()
        .lock()
        .unwrap_or_else(|error| error.into_inner());
    let presented = presented.entry(context as usize).or_default();
    for source in &sampled {
        if !presented.contains(&source.texture) {
            presented.push(source.texture);
        }
    }
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

#[cfg(test)]
mod tests {
    use super::{
        DrawCandidate, QuadBounds, TargetKey, intersect, select_from_dependency_graph,
        select_screen_cohort,
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
            vec![10, 20],
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
            vec![10, 20],
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
