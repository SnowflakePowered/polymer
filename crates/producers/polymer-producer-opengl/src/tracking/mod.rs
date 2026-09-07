//! Heuristics for finding the game texture and its presented bounds.

mod contexts;
mod renderbuffers;
mod selection;
mod transfers;
pub use contexts::Tracking;

use crate::interop::{
    GL_ACTIVE_TEXTURE, GL_DRAW_FRAMEBUFFER, GL_DRAW_FRAMEBUFFER_BINDING, GL_READ_FRAMEBUFFER,
    GL_READ_FRAMEBUFFER_BINDING, GL_SCISSOR_BOX, GL_SCISSOR_TEST, GL_TEXTURE_2D,
    GL_TEXTURE_2D_ARRAY, GL_TEXTURE_BINDING_2D, GL_TEXTURE_BINDING_2D_ARRAY, GL_TEXTURE_HEIGHT,
    GL_TEXTURE_WIDTH, GL_TEXTURE0, GL_VIEWPORT, GlActiveTexture, Glenum, Gluint, current_context,
    glBindTexture, glGetIntegerv, glGetTexLevelParameteriv, glIsEnabled, proc,
};
use parking_lot::Mutex;
use std::{
    ffi::c_void,
    sync::{
        OnceLock,
        atomic::{AtomicU64, Ordering},
    },
};

pub(crate) type SurfaceIdentity = (u32, u64, i32, u32, i32);

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(crate) struct QuadBounds {
    pub(crate) x: i32,
    pub(crate) y: i32,
    pub(crate) width: u32,
    pub(crate) height: u32,
}

#[derive(Clone, Copy)]
pub(crate) struct DrawCandidate {
    pub(crate) selection: polymer_presentation::SurfaceSelection,
    pub(crate) mip: i32,
    pub(crate) incarnation: u64,

    pub(crate) texture: Gluint,
    pub(crate) target: Glenum,
    pub(crate) layer: i32,
    pub(crate) width: u32,
    pub(crate) height: u32,
}

impl DrawCandidate {
    fn valid_crop(&self) -> bool {
        let extent = polymer_presentation::Extent2D {
            width: self.width,
            height: self.height,
        };
        self.selection.source_rect.intersect(extent.rect()) == Some(self.selection.source_rect)
    }
}

impl polymer_presentation::history::HistorySurface for DrawCandidate {
    fn selection(&mut self) -> &mut polymer_presentation::SurfaceSelection {
        &mut self.selection
    }
    type Id = SurfaceIdentity;
    type Layout = (u32, i32, i32, u32, u32);
    fn identity(&self) -> Self::Id {
        (
            self.texture,
            self.incarnation,
            self.mip,
            self.target,
            self.layer,
        )
    }
    fn layout(&self) -> Self::Layout {
        (
            self.target,
            self.mip,
            self.layer,
            self.selection.source_rect.extent.width,
            self.selection.source_rect.extent.height,
        )
    }
}

#[derive(Clone, Copy, Debug, Eq, Hash, PartialEq)]
struct TargetKey {
    renderbuffer: bool,
    mip: i32,
    incarnation: u64,
    texture: Gluint,
    layer: i32,
}

impl DrawCandidate {
    fn key(self) -> TargetKey {
        TargetKey {
            renderbuffer: self.target == GL_RENDERBUFFER,
            mip: self.mip,
            incarnation: self.incarnation,
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
struct ContextTracking {
    backbuffer: Mutex<Option<DrawCandidate>>,
    drawable_incarnations: dashmap::DashMap<u64, u64>,
    incarnations: dashmap::DashMap<(bool, Gluint), u64>,
    next_incarnation: AtomicU64,
    observations: polymer_presentation::ObservationStore<TargetKey>,
    observation_order: AtomicU64,
    resources: dashmap::DashMap<TargetKey, DrawCandidate>,
    texture_info: dashmap::DashMap<Gluint, (Glenum, u32, u32)>,
    active_texture: OnceLock<GlActiveTexture>,
    get_framebuffer_attachment: OnceLock<GlGetFramebufferAttachmentParameteriv>,
    get_texture_level_parameter: OnceLock<Option<GlGetTextureLevelParameteriv>>,
    quad_reader: OnceLock<Option<QuadReader>>,
    quad_logs: AtomicU64,
}

const GL_FRAMEBUFFER_ATTACHMENT_OBJECT_TYPE: Glenum = 0x8CD0;
const GL_FRAMEBUFFER_ATTACHMENT_OBJECT_NAME: Glenum = 0x8CD1;
const GL_FRAMEBUFFER_ATTACHMENT_TEXTURE_LAYER: Glenum = 0x8CD4;
const GL_FRAMEBUFFER_ATTACHMENT_TEXTURE_LEVEL: Glenum = 0x8CD2;
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

impl ContextTracking {
    fn incarnation(&self, renderbuffer: bool, texture: Gluint) -> u64 {
        *self
            .incarnations
            .entry((renderbuffer, texture))
            .or_insert_with(|| self.next_incarnation.fetch_add(1, Ordering::Relaxed))
    }
    fn generation(&self) -> u64 {
        self.observations.generation()
    }

    fn upsert_render_target(&self, candidate: DrawCandidate) {
        self.resources.insert(candidate.key(), candidate);
    }

    fn record_dependencies(&self, key: TargetKey, sources: &[Gluint], order: u64) {
        let targets = &self.resources;
        for &texture in sources {
            let mut ids = targets
                .iter()
                .filter(|candidate| {
                    candidate.value().texture == texture
                        && candidate.value().target != GL_RENDERBUFFER
                })
                .map(|candidate| *candidate.key())
                .collect::<Vec<_>>();
            if ids.is_empty() {
                ids.push(TargetKey {
                    renderbuffer: false,
                    texture,
                    incarnation: self.incarnation(false, texture),
                    mip: 0,
                    layer: 0,
                });
            }
            ids.sort_unstable_by_key(|id| (id.incarnation, id.mip, id.layer));
            for source in ids {
                if source != key {
                    self.observations
                        .record(polymer_presentation::DependencyEdge {
                            order,
                            ..polymer_presentation::DependencyEdge::sampled(
                                source,
                                key,
                                self.generation(),
                            )
                        });
                }
            }
        }
    }
}

impl QuadBounds {
    fn intersect_raster(a: [i32; 4], b: [i32; 4]) -> Option<[i32; 4]> {
        let rect = |[x, y, width, height]: [i32; 4]| {
            Some(polymer_presentation::Rect {
                x,
                y,
                extent: polymer_presentation::Extent2D {
                    width: u32::try_from(width).ok()?,
                    height: u32::try_from(height).ok()?,
                },
            })
        };
        let result = rect(a)?.intersect(rect(b)?)?;
        Some([
            result.x,
            result.y,
            result.extent.width.try_into().ok()?,
            result.extent.height.try_into().ok()?,
        ])
    }
}

/// Returns the selected capture sources and whether they came from a trusted
/// blit-presented graph resolution. Trusted selections are complete: the
/// caller must not pad them from history.
impl ContextTracking {
    pub(crate) fn take_draw_candidates(
        &self,
        backbuffer: Option<DrawCandidate>,
    ) -> (Vec<DrawCandidate>, bool) {
        let backbuffer = backbuffer.map(|mut source| {
            source.incarnation = *self
                .drawable_incarnations
                .entry(source.incarnation)
                .or_insert_with(|| self.next_incarnation.fetch_add(1, Ordering::Relaxed));
            source
        });
        *self.backbuffer.lock() = backbuffer;
        let Some(limit) = polymer_hook::config::tracked_render_target_count() else {
            return (Vec::new(), false);
        };
        self.select(
            backbuffer,
            limit,
            polymer_hook::config::graph_tracking_enabled(),
        )
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
    /// direct-state-access queries are safe. Older contexts use binding-based
    /// queries and restore the application's binding before returning.
    pub(crate) fn refresh_sources(&self, sources: &[DrawCandidate]) -> Vec<DrawCandidate> {
        if sources.is_empty() {
            return Vec::new();
        }
        let query = *self
            .get_texture_level_parameter
            .get_or_init(|| unsafe { proc(b"glGetTextureLevelParameteriv\0") });
        sources
            .iter()
            .filter(|source| {
                if source.target == 0 && source.texture == 0 {
                    return source.valid_crop()
                        && self.backbuffer.lock().as_ref().is_some_and(|current| {
                            current.incarnation == source.incarnation
                                && (source.width, source.height) == (current.width, current.height)
                        });
                }
                if source.target == GL_RENDERBUFFER {
                    return self.refresh_renderbuffer(source);
                }
                if !source.valid_crop()
                    || self.incarnation(false, source.texture) != source.incarnation
                {
                    return false;
                }
                let (width, height, layers) = unsafe { source.measure_dimensions(query) };
                if source.layer < 0 || source.layer >= layers {
                    return false;
                }
                // A deleted texture answers 0; a live one answers its current
                // extent, which the cache is updated to match so downstream
                // measurements and the merge agree on the reallocated size.
                if width <= 0 || height <= 0 {
                    self.texture_info.remove(&source.texture);
                    return false;
                }
                if source.mip == 0
                    && let Some(mut entry) = self.texture_info.get_mut(&source.texture)
                {
                    entry.1 = width as u32;
                    entry.2 = height as u32;
                }
                (width as u32, height as u32) == (source.width, source.height)
            })
            .copied()
            .map(|mut source| {
                if let Some(window) = self.backbuffer.lock().as_ref() {
                    let extent = polymer_presentation::Extent2D {
                        width: window.width,
                        height: window.height,
                    };
                    if source.selection.target_extent != extent {
                        source.selection.target_extent = extent;
                        source.selection.presentation_rect = None;
                    }
                }
                source
            })
            .collect()
    }
}

impl DrawCandidate {
    unsafe fn measure_dimensions(
        &self,
        query: Option<GlGetTextureLevelParameteriv>,
    ) -> (i32, i32, i32) {
        let (mut width, mut height, mut layers) = (0, 0, 1);
        let mut measure = |query: &mut dyn FnMut(u32, &mut i32)| {
            query(GL_TEXTURE_WIDTH, &mut width);
            query(GL_TEXTURE_HEIGHT, &mut height);
            if self.target == GL_TEXTURE_2D_ARRAY {
                query(0x8071 /* GL_TEXTURE_DEPTH */, &mut layers);
            }
        };
        if let Some(query) = query {
            measure(&mut |property, value| unsafe {
                query(self.texture, self.mip, property, value)
            });
        } else {
            if unsafe { crate::interop::glIsTexture(self.texture) } == 0 {
                return (0, 0, 0);
            }
            let binding = match self.target {
                GL_TEXTURE_2D => GL_TEXTURE_BINDING_2D,
                GL_TEXTURE_2D_ARRAY => GL_TEXTURE_BINDING_2D_ARRAY,
                _ => return (0, 0, 0),
            };
            let mut old = 0;
            unsafe {
                glGetIntegerv(binding, &mut old);
                glBindTexture(self.target, self.texture);
            }
            measure(&mut |property, value| unsafe {
                glGetTexLevelParameteriv(self.target, self.mip, property, value)
            });
            unsafe { glBindTexture(self.target, old as u32) };
        }
        (width, height, layers)
    }
}

/// Records sampled textures, framebuffer destinations, and the draw's raster bounds.
impl ContextTracking {
    pub unsafe fn record_draw_candidate(&self, arrays_range: Option<(i32, i32)>) {
        if polymer_hook::config::tracked_render_target_count().is_none() {
            return;
        }
        let context = unsafe { current_context() };
        if context.is_null() {
            return;
        }
        let order = self.observation_order.fetch_add(1, Ordering::Relaxed);
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
            QuadBounds::intersect_raster(viewport, scissor)
        } else {
            (viewport[2] > 0 && viewport[3] > 0).then_some(viewport)
        };
        let Some(raster) = raster else { return };
        let bounds = QuadBounds {
            x: raster[0],
            y: raster[1],
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
                if width <= 0 || height <= 0 {
                    continue;
                }
                sampled.push(DrawCandidate {
                    selection: Default::default(),
                    mip: 0,
                    incarnation: self.incarnation(false, texture as u32),

                    texture: texture as u32,
                    target,
                    layer: 0,
                    width: width as u32,
                    height: height as u32,
                });
                self.texture_info
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
                let mut mip = 0;
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
                    if object_type == GL_TEXTURE as i32 {
                        function(
                            GL_DRAW_FRAMEBUFFER,
                            attachment as u32,
                            GL_FRAMEBUFFER_ATTACHMENT_TEXTURE_LAYER,
                            &mut layer,
                        );
                        function(
                            GL_DRAW_FRAMEBUFFER,
                            attachment as u32,
                            GL_FRAMEBUFFER_ATTACHMENT_TEXTURE_LEVEL,
                            &mut mip,
                        );
                    }
                }
                if texture <= 0 {
                    continue;
                }
                if object_type == GL_RENDERBUFFER as i32 {
                    if let Some(candidate) = unsafe { self.renderbuffer_candidate(texture as u32) }
                    {
                        self.upsert_render_target(candidate);
                        self.observations.target(
                            polymer_presentation::Target {
                                id: candidate.key(),
                                score: raster_area,
                                order,
                            },
                            self.generation(),
                        );
                        self.record_dependencies(candidate.key(), &sampled_names, order);
                    }
                    continue;
                }
                if object_type != GL_TEXTURE as i32 {
                    continue;
                }
                let candidate = DrawCandidate {
                    selection: Default::default(),
                    mip,
                    incarnation: self.incarnation(false, texture as u32),

                    texture: texture as u32,
                    target: 0,
                    layer,
                    width: bounds.width,
                    height: bounds.height,
                };
                self.upsert_render_target(candidate);
                self.observations.target(
                    polymer_presentation::Target {
                        id: candidate.key(),
                        score: raster_area,
                        order,
                    },
                    self.generation(),
                );
                self.record_dependencies(candidate.key(), &sampled_names, order);
            }
            return;
        }
        let mut raster = polymer_presentation::DependencyEdge::sampled(
            TargetKey::PRESENT,
            TargetKey::PRESENT,
            self.generation(),
        );
        raster.order = order;
        raster.coverage = Some(polymer_presentation::Rect {
            x: bounds.x,
            y: bounds.y,
            extent: polymer_presentation::Extent2D {
                width: bounds.width,
                height: bounds.height,
            },
        });
        self.observations.record(raster);
        if !self.record_present_sources(sampled, present_quad, order) {
            self.record_dependencies(TargetKey::PRESENT, &sampled_names, order);
        }
    }

    fn record_present_sources(
        &self,
        mut sampled: Vec<DrawCandidate>,
        present_quad: Option<QuadBounds>,
        order: u64,
    ) -> bool {
        use polymer_presentation::{DependencyEdge, Extent2D, Rect, RegionMapping};
        let mut seen = std::collections::HashSet::new();
        sampled.retain(|candidate| seen.insert(candidate.key()));
        // Native metadata does not establish rendered-content evidence.
        for &candidate in &sampled {
            self.upsert_render_target(candidate);
        }
        if let (Some(quad), [candidate]) = (present_quad, sampled.as_slice()) {
            if self.resources.iter().any(|other| {
                other.texture == candidate.texture
                    && other.target != GL_RENDERBUFFER
                    && *other.key() != candidate.key()
            }) {
                return false;
            }
            let mut edge =
                DependencyEdge::sampled(candidate.key(), TargetKey::PRESENT, self.generation());
            edge.order = order;
            // Position recovery supplies placement, not proof that every
            // sampled texture is a rendered screen. Keep sampled-draw policy.
            edge.mapping = Some(RegionMapping {
                source: Extent2D {
                    width: candidate.width,
                    height: candidate.height,
                }
                .rect(),
                destination: Rect {
                    x: quad.x,
                    y: quad.y,
                    extent: Extent2D {
                        width: quad.width,
                        height: quad.height,
                    },
                },
                flip_x: false,
                flip_y: false,
            });
            edge.coverage = edge.mapping.map(|mapping| mapping.destination);
            self.observations.record(edge);
            return true;
        }
        false
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
        let coordinates = [
            lo.0 / layout_width * width,
            lo.1 / layout_height * height,
            hi.0 / layout_width * width,
            hi.1 / layout_height * height,
        ]
        .map(|value| f64::from(value).round());
        if coordinates.iter().any(|value| {
            !value.is_finite() || *value < f64::from(i32::MIN) || *value > f64::from(i32::MAX)
        }) {
            return None;
        }
        let [left, top, right, bottom] = coordinates.map(|value| value as i64);
        if right <= left || bottom <= top {
            return None;
        }
        Some(QuadBounds {
            x: i32::try_from(i64::from(viewport[0]) + left).ok()?,
            y: i32::try_from(i64::from(viewport[1]) + i64::from(viewport[3]) - bottom).ok()?,
            width: u32::try_from(right - left).ok()?,
            height: u32::try_from(bottom - top).ok()?,
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
        if let Some(info) = self.texture_info.get(&texture) {
            let (_, width, height) = *info;
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
        if width <= 0 || height <= 0 {
            return None;
        }
        self.texture_info
            .insert(texture, (target, width as u32, height as u32));
        Some((width as u32, height as u32))
    }
}

#[cfg(test)]
mod tests {
    use super::{ContextTracking, DrawCandidate, QuadBounds};

    #[test]
    fn recovered_quad_requires_one_distinct_sampled_source() {
        let tracking = ContextTracking::default();
        let first = candidate(1, 320, 240);
        let second = candidate(2, 640, 480);
        tracking.record_present_sources(
            vec![first, second, first],
            Some(QuadBounds {
                x: 0,
                y: 0,
                width: 640,
                height: 480,
            }),
            1,
        );
        assert!(tracking.observations.graph(0).edges.is_empty());
        tracking.record_present_sources(
            vec![first, first],
            Some(QuadBounds {
                x: 0,
                y: 0,
                width: 640,
                height: 480,
            }),
            2,
        );
        let graph = tracking.observations.graph(0);
        let edge = graph.edges.values().flatten().next().unwrap();
        assert!(edge.mapping.is_some());
        assert!(!edge.exact());
        assert_eq!(edge.source, first.key());
        assert_eq!(edge.order, 2);
        tracking.observations.clear();
        let sibling = DrawCandidate { layer: 1, ..first };
        tracking.resources.insert(sibling.key(), sibling);
        assert!(!tracking.record_present_sources(
            vec![first],
            Some(QuadBounds {
                x: 0,
                y: 0,
                width: 640,
                height: 480,
            }),
            3
        ));
        assert!(tracking.observations.graph(0).edges.is_empty());
    }

    pub(super) fn candidate(texture: u32, width: u32, height: u32) -> DrawCandidate {
        DrawCandidate {
            selection: polymer_presentation::SurfaceSelection {
                source_rect: polymer_presentation::Extent2D { width, height }.rect(),
                ..Default::default()
            },
            mip: 0,
            incarnation: 0,

            texture,
            target: 0x0de1,
            layer: 0,
            width,
            height,
        }
    }

    #[test]
    fn array_revalidation_drops_removed_layers_even_when_extent_is_unchanged() {
        unsafe extern "system" fn query(_: u32, _: i32, property: u32, value: *mut i32) {
            unsafe { *value = if property == 0x8071 { 1 } else { 64 } };
        }
        let tracking = ContextTracking::default();
        tracking
            .get_texture_level_parameter
            .set(Some(query))
            .unwrap();
        let mut source = candidate(1, 64, 64);
        source.target = super::GL_TEXTURE_2D_ARRAY;
        source.incarnation = tracking.incarnation(false, source.texture);
        let mut removed_layer = source;
        removed_layer.layer = 1;
        let retained = tracking.refresh_sources(&[source, removed_layer]);
        assert_eq!(retained.len(), 1);
        assert_eq!(retained[0].layer, 0);
    }

    #[test]
    fn present_draws_keep_recording_stamps_and_distinct_known_subresources() {
        use super::TargetKey;
        let tracking = ContextTracking::default();
        for _ in 0..7 {
            tracking.observations.present(true);
        }
        let first = candidate(3, 320, 240);
        let mut second = first;
        second.layer = 1;
        for source in [first, second] {
            tracking.upsert_render_target(source);
        }
        tracking.record_dependencies(TargetKey::PRESENT, &[3], 42);
        let observations = &tracking.observations;
        let (_, edges) = observations.graph(8).observation_list();
        assert_eq!(edges.len(), 2);
        assert!(
            edges
                .iter()
                .all(|edge| edge.generation == 7 && edge.order == 42)
        );
        assert_eq!(
            edges.iter().map(|edge| edge.source).collect::<Vec<_>>(),
            [first.key(), second.key()]
        );
        assert!(
            edges
                .iter()
                .all(|edge| edge.mapping.is_none() && !edge.whole_source)
        );
        tracking.observations.present(true);
        tracking.record_dependencies(TargetKey::PRESENT, &[3], 43);
        let (_, edges) = tracking.observations.graph(9).observation_list();
        assert_eq!(edges.len(), 2);
        assert!(
            edges
                .iter()
                .all(|edge| edge.generation == 8 && edge.order == 43)
        );
    }

    #[test]
    fn deletion_clears_pending_observations_without_disturbing_other_resources_or_contexts() {
        let first = ContextTracking::default();
        let second = ContextTracking::default();
        first.texture_info.insert(7, (0x0de1, 640, 480));
        second.texture_info.insert(7, (0x0de1, 320, 240));
        let deleted = candidate(7, 640, 480);
        let sibling = candidate(8, 320, 240);
        first
            .observations
            .record(polymer_presentation::DependencyEdge::sampled(
                deleted.key(),
                sibling.key(),
                1,
            ));
        first.forget_texture(7);
        assert!(!first.texture_info.contains_key(&7));
        assert_eq!(*second.texture_info.get(&7).unwrap(), (0x0de1, 320, 240));
        assert!(first.observations.graph(1).observation_list().1.is_empty());
    }

    #[test]
    fn scissor_is_intersected_with_viewport() {
        assert_eq!(
            QuadBounds::intersect_raster([i32::MAX - 8, 0, 32, 10], [i32::MAX - 4, 0, 16, 10]),
            Some([i32::MAX - 4, 0, 16, 10]),
        );
        assert_eq!(
            QuadBounds::intersect_raster([100, 50, 800, 600], [0, 120, 1920, 300]),
            Some([100, 120, 800, 300])
        );
        assert_eq!(
            QuadBounds::intersect_raster([0, 0, 100, 100], [200, 200, 10, 10]),
            None
        );
    }
}
