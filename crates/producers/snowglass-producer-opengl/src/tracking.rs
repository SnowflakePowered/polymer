//! Heuristics for finding the game texture and its presented bounds.

use crate::interop::{
    GL_ACTIVE_TEXTURE, GL_DRAW_FRAMEBUFFER_BINDING, GL_SCISSOR_BOX, GL_SCISSOR_TEST, GL_TEXTURE_2D,
    GL_TEXTURE_2D_ARRAY, GL_TEXTURE_BINDING_2D, GL_TEXTURE_BINDING_2D_ARRAY, GL_TEXTURE_HEIGHT,
    GL_TEXTURE_WIDTH, GL_TEXTURE0, GL_VIEWPORT, GlActiveTexture, Glenum, Gluint, current_context,
    glGetIntegerv, glGetTexLevelParameteriv, glIsEnabled, proc,
};
use std::{
    collections::HashMap,
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
    pub(crate) width: u32,
    pub(crate) height: u32,
    pub(crate) bounds: QuadBounds,
    score: u64,
}

static DRAW_CANDIDATES: OnceLock<Mutex<HashMap<usize, DrawCandidate>>> = OnceLock::new();
static ACTIVE_TEXTURE: OnceLock<GlActiveTexture> = OnceLock::new();

fn draw_candidates() -> &'static Mutex<HashMap<usize, DrawCandidate>> {
    DRAW_CANDIDATES.get_or_init(|| Mutex::new(HashMap::new()))
}

fn intersect(a: [i32; 4], b: [i32; 4]) -> Option<[i32; 4]> {
    let left = a[0].max(b[0]);
    let bottom = a[1].max(b[1]);
    let right = a[0].saturating_add(a[2]).min(b[0].saturating_add(b[2]));
    let top = a[1].saturating_add(a[3]).min(b[1].saturating_add(b[3]));
    (right > left && top > bottom).then_some([left, bottom, right - left, top - bottom])
}

pub(crate) fn take_draw_candidate(context: usize) -> Option<DrawCandidate> {
    if std::env::var_os("SNOWGLASS_TRACK_RENDER_TARGET").is_none() {
        return None;
    }
    draw_candidates()
        .lock()
        .unwrap_or_else(|error| error.into_inner())
        .remove(&context)
}

/// Samples textures and the actual raster bounds for a draw to the default framebuffer.
pub unsafe fn record_draw_candidate() {
    if std::env::var_os("SNOWGLASS_TRACK_RENDER_TARGET").is_none() {
        return;
    }
    let context = unsafe { current_context() };
    if context.is_null() {
        return;
    }
    let mut draw_framebuffer = 0;
    unsafe { glGetIntegerv(GL_DRAW_FRAMEBUFFER_BINDING, &mut draw_framebuffer) };
    if draw_framebuffer != 0 {
        return;
    }
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
    let mut best: Option<DrawCandidate> = None;
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
            if width > 1 && height > 1 {
                let cross_a = width as u64 * u64::from(bounds.height);
                let cross_b = height as u64 * u64::from(bounds.width);
                let aspect_match = cross_a
                    .min(cross_b)
                    .saturating_mul(1024)
                    .checked_div(cross_a.max(cross_b))
                    .unwrap_or(0);
                let candidate = DrawCandidate {
                    texture: texture as u32,
                    target,
                    width: width as u32,
                    height: height as u32,
                    bounds,
                    score: raster_area
                        .saturating_mul(width as u64 * height as u64)
                        .saturating_mul(aspect_match),
                };
                if best.is_none_or(|current| candidate.score > current.score) {
                    best = Some(candidate);
                }
            }
        }
    }
    unsafe { active_texture(old_active as u32) };
    if let Some(candidate) = best {
        let mut candidates = draw_candidates()
            .lock()
            .unwrap_or_else(|error| error.into_inner());
        if candidates
            .get(&(context as usize))
            .is_none_or(|current| candidate.score > current.score)
        {
            candidates.insert(context as usize, candidate);
        }
    }
}

#[cfg(test)]
mod tests {
    use super::intersect;

    #[test]
    fn scissor_is_intersected_with_viewport() {
        assert_eq!(
            intersect([100, 50, 800, 600], [0, 120, 1920, 300]),
            Some([100, 120, 800, 300])
        );
        assert_eq!(intersect([0, 0, 100, 100], [200, 200, 10, 10]), None);
    }
}
