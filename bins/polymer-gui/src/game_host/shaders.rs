use super::*;
use crate::shell::protocol::ShaderTarget;
use polymer_composition::{ShaderControl, SurfaceShader};

/// A shader preset attached to a shell element, the group or stream that
/// presents the element, and whether that presenter accepted the shader.
pub(super) struct ShaderAttachment {
    shader: SurfaceShader,
    presenter: Option<u64>,
    applied: bool,
}

impl ShaderAttachment {
    fn applied_presenter(&self) -> Option<u64> {
        self.presenter.filter(|_| self.applied)
    }
}

impl GameHost {
    pub fn control_shader(&mut self, target: ShaderTarget, control: ShaderControl) {
        match control {
            ShaderControl::Attach(shader) => self.attach_shader(target, *shader),
            ShaderControl::Detach => {
                if let Some(presenter) = self
                    .shaders
                    .remove(&target)
                    .and_then(|attachment| attachment.applied_presenter())
                {
                    self.compositor
                        .borrow_mut()
                        .control_shader(presenter, ShaderControl::Detach);
                }
            }
            ShaderControl::Parameter(name, value) => {
                if let Some(attachment) = self.shaders.get_mut(&target) {
                    let parameters = &mut attachment.shader.parameters;
                    parameters.retain(|(existing, _)| *existing != name);
                    parameters.push((name.clone(), value));
                }
                self.forward_shader(target, ShaderControl::Parameter(name, value));
                return;
            }
            ShaderControl::Passes(passes) => {
                if let Some(attachment) = self.shaders.get_mut(&target) {
                    attachment.shader.passes = Some(passes);
                }
                self.forward_shader(target, ShaderControl::Passes(passes));
                return;
            }
        }
        self.sync_surface_shaders();
        self.mark_composition_dirty();
    }

    fn attach_shader(&mut self, target: ShaderTarget, shader: SurfaceShader) {
        let presenter = self
            .shaders
            .remove(&target)
            .and_then(|previous| previous.applied_presenter());
        let applied = presenter.is_some_and(|presenter| {
            self.compositor
                .borrow_mut()
                .control_shader(presenter, ShaderControl::Attach(Box::new(shader.clone())))
        });
        self.shaders.insert(
            target,
            ShaderAttachment {
                shader,
                presenter,
                applied,
            },
        );
    }

    fn forward_shader(&self, target: ShaderTarget, control: ShaderControl) {
        if let Some(presenter) = self
            .shaders
            .get(&target)
            .and_then(ShaderAttachment::applied_presenter)
        {
            self.compositor
                .borrow_mut()
                .control_shader(presenter, control);
        }
    }

    fn shader_presenter(&self, target: ShaderTarget) -> Option<u64> {
        match target {
            ShaderTarget::Group(group) => self.surfaces.values().find_map(|surface| {
                surface.surface_group.filter(|_| {
                    surface.geometries.iter().any(|geometry| {
                        geometry
                            .surface_group
                            .is_some_and(|surface_group| surface_group.id == group)
                    })
                })
            }),
            ShaderTarget::Surface(node) => self
                .surfaces
                .iter()
                .find(|(_, surface)| {
                    surface.surface_group.is_none()
                        && surface
                            .geometries
                            .iter()
                            .any(|geometry| geometry.node == node)
                })
                .map(|(&connection, _)| connection),
        }
    }

    /// The viewport of the shader this stream presents as a group of one, if any.
    pub(super) fn shaded_alone(&self, connection: u64) -> Option<ShaderViewport> {
        self.surfaces
            .get(&connection)
            .filter(|surface| surface.surface_group.is_none())?;
        self.shaders
            .values()
            .find(|attachment| attachment.applied_presenter() == Some(connection))
            .map(|attachment| attachment.shader.viewport)
    }

    /// Where a shaded presenter actually drew this placement, in window pixels:
    /// the composed surfaces' union maps onto the measured shader content.
    pub(super) fn shaded_destination(
        &self,
        connection: u64,
        surface: &CapturedSurface,
        geometry: &SurfaceGeometry,
        placement: &SurfacePlacement,
    ) -> Option<[f32; 4]> {
        let (presenter, origin) = match (surface.surface_group, self.shaded_alone(connection)) {
            (Some(group), _) => (group, geometry.surface_group?.bounds),
            (None, Some(viewport)) => (connection, surface.shaded_box(geometry, viewport)),
            (None, None) => return None,
        };
        let content = self.shader_contents.get(&presenter)?;
        let union = self
            .surface_group_layouts
            .get(&presenter)?
            .members
            .iter()
            .map(|member| member.placement.destination)
            .reduce(|[l0, t0, r0, b0], [l1, t1, r1, b1]| {
                [l0.min(l1), t0.min(t1), r0.max(r1), b0.max(b1)]
            })?;
        let scale = [
            (content[2] - content[0]) / (union[2] - union[0]).max(1.0),
            (content[3] - content[1]) / (union[3] - union[1]).max(1.0),
        ];
        let (x, y) = (origin.x as f32, origin.y as f32);
        let map = |value: f32, axis: usize, offset: f32| {
            offset + content[axis] + (value - offset - union[axis]) * scale[axis]
        };
        let [left, top, right, bottom] = placement.content.destination;
        Some([
            map(left, 0, x),
            map(top, 1, y),
            map(right, 0, x),
            map(bottom, 1, y),
        ])
    }

    /// Moves each attachment onto whichever group or stream presents its element now.
    pub(super) fn sync_surface_shaders(&mut self) {
        let moves = self
            .shaders
            .iter()
            .map(|(&target, attachment)| (target, attachment.presenter))
            .map(|(target, presenter)| (target, presenter, self.shader_presenter(target)))
            .filter(|(_, presenter, resolved)| presenter != resolved)
            .collect::<Vec<_>>();
        for (target, _, resolved) in moves {
            let attachment = self.shaders.get_mut(&target).expect("attachment exists");
            let mut compositor = self.compositor.borrow_mut();
            if let Some(presenter) = attachment.applied_presenter() {
                compositor.control_shader(presenter, ShaderControl::Detach);
            }
            attachment.applied = resolved.is_some_and(|resolved| {
                compositor.control_shader(
                    resolved,
                    ShaderControl::Attach(Box::new(attachment.shader.clone())),
                )
            });
            attachment.presenter = resolved;
        }
        self.arrange_surface_groups();
    }

    pub(super) fn forget_shader_presenters(&mut self) {
        self.shader_contents.clear();
        for attachment in self.shaders.values_mut() {
            attachment.presenter = None;
            attachment.applied = false;
        }
    }

    /// Drops every attachment, as when the shell document that owned them is replaced.
    pub fn clear_shaders(&mut self) {
        self.shader_contents.clear();
        for presenter in self
            .shaders
            .drain()
            .filter_map(|(_, attachment)| attachment.applied_presenter())
        {
            self.compositor
                .borrow_mut()
                .control_shader(presenter, ShaderControl::Detach);
        }
        self.sync_surface_shaders();
        self.mark_composition_dirty();
    }
}
