use super::*;
use polymer_presentation::{
    DependencyEdge, Evidence, Extent2D, ObservationStore, Rect, RegionMapping,
};

#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub(super) struct SurfaceId {
    pub image: vk::Image,
    pub incarnation: u64,
    pub mip: u32,
    pub layer: u32,
}

#[derive(Default)]
pub(super) struct Surfaces {
    pub shapes: DashMap<vk::Image, (u32, u32)>,
    pub observations: ObservationStore<SurfaceId>,
    pub layouts: DashMap<SurfaceId, vk::ImageLayout>,
    pub order: AtomicU64,
    incarnations: DashMap<vk::Image, u64>,
    next: AtomicU64,
}

impl Tracking {
    pub fn image_subresources(&self, image: vk::Image, mip_levels: u32, layers: u32) {
        self.surfaces.shapes.insert(image, (mip_levels, layers));
    }

    pub(super) fn view_surfaces(
        &self,
        image: vk::Image,
        range: vk::ImageSubresourceRange,
    ) -> Vec<SurfaceId> {
        let (levels, layers) = self
            .surfaces
            .shapes
            .get(&image)
            .map_or((1, 1), |shape| *shape);
        (range.base_mip_level
            ..range
                .base_mip_level
                .saturating_add(range.level_count)
                .min(levels))
            .flat_map(|mip| {
                (range.base_array_layer
                    ..range
                        .base_array_layer
                        .saturating_add(range.layer_count)
                        .min(layers))
                    .map(move |layer| self.surface_id(image, mip, layer))
            })
            .collect()
    }

    pub(super) fn submit_surface_draw(&self, state: &CommandTrackingState) {
        let order = self.surfaces.order.fetch_add(1, Ordering::Relaxed);
        let Some(framebuffer) = state.framebuffer else {
            return;
        };
        let Some(targets) = self.framebuffer_images.get(&framebuffer) else {
            return;
        };
        let mut sampled = state.pushed_images.clone();
        for set in &state.descriptor_sets {
            if let Some(images) = self.descriptor_set_images.get(set) {
                for (ids, layout) in images.values() {
                    sampled.extend(ids.iter().map(|id| (*id, *layout)));
                }
            }
        }
        for &destination in targets.iter() {
            let Some(extent) = self.surface_extent(destination) else {
                continue;
            };
            let Some(coverage) = raster_rect(
                state.viewport,
                state.scissor,
                vk::Extent2D {
                    width: extent.width,
                    height: extent.height,
                },
            ) else {
                continue;
            };
            let mut raster = DependencyEdge::sampled(destination, destination, self.generation());
            raster.order = order;
            raster.coverage = Some(Rect {
                x: coverage.offset.x,
                y: coverage.offset.y,
                extent: Extent2D {
                    width: coverage.extent.width,
                    height: coverage.extent.height,
                },
            });
            self.surfaces.observations.record(raster);
            self.surfaces.observations.target(
                polymer_presentation::Target {
                    id: destination,
                    score: u64::from(coverage.extent.width) * u64::from(coverage.extent.height),
                    order,
                },
                self.generation(),
            );
            for &(source, layout) in &sampled {
                if source == destination {
                    continue;
                }
                let mut edge = DependencyEdge::sampled(source, destination, 0);
                edge.coverage = Some(Rect {
                    x: coverage.offset.x,
                    y: coverage.offset.y,
                    extent: Extent2D {
                        width: coverage.extent.width,
                        height: coverage.extent.height,
                    },
                });
                self.submit_surface_edge(edge, layout, order);
            }
        }
    }

    pub(super) fn submit_surface_edge(
        &self,
        mut edge: DependencyEdge<SurfaceId>,
        layout: vk::ImageLayout,
        order: u64,
    ) {
        if self.surface_extent(edge.source).is_none()
            || self.surface_extent(edge.destination).is_none()
        {
            return;
        }
        edge.generation = self.generation();
        edge.order = order;
        self.surfaces.layouts.insert(edge.source, layout);
        self.surfaces.observations.record(edge);
    }
    pub(super) fn surface_id(&self, image: vk::Image, mip: u32, layer: u32) -> SurfaceId {
        let incarnation = *self
            .surfaces
            .incarnations
            .entry(image)
            .or_insert_with(|| self.surfaces.next.fetch_add(1, Ordering::Relaxed));
        SurfaceId {
            image,
            incarnation,
            mip,
            layer,
        }
    }

    pub(super) fn surface_extent(&self, id: SurfaceId) -> Option<Extent2D> {
        let (levels, layers) = self
            .surfaces
            .shapes
            .get(&id.image)
            .map_or((1, 1), |shape| *shape);
        if id.mip >= levels || id.layer >= layers {
            return None;
        }
        if self
            .surfaces
            .incarnations
            .get(&id.image)
            .is_none_or(|value| *value != id.incarnation)
        {
            return None;
        }
        let extent = self
            .image_extent(id.image)
            .or_else(|| self.swapchain_extent(id.image))?;
        Some(Extent2D {
            width: extent.width.checked_shr(id.mip)?.max(1),
            height: extent.height.checked_shr(id.mip)?.max(1),
        })
    }

    pub(super) fn forget_surfaces(&self, image: vk::Image) {
        self.surfaces.shapes.remove(&image);
        self.surfaces.incarnations.remove(&image);
        self.surfaces
            .observations
            .forget_matching(|id| id.image == image);
        self.surfaces.layouts.retain(|id, _| id.image != image);
    }

    /// Copies and resolves share an unscaled region representation; blits
    /// normalize endpoint orientation in the native hook adapter.
    pub fn command_image_transferred(
        &self,
        command: vk::CommandBuffer,
        source: vk::Image,
        layout: vk::ImageLayout,
        destination: vk::Image,
        region: &vk::ImageCopy,
        evidence: Evidence,
    ) {
        let extent = vk::Extent2D {
            width: region.extent.width,
            height: region.extent.height,
        };
        self.command_transferred(
            command,
            source,
            region.src_subresource,
            layout,
            destination,
            region.dst_subresource,
            vk::Rect2D {
                offset: vk::Offset2D {
                    x: region.src_offset.x,
                    y: region.src_offset.y,
                },
                extent,
            },
            vk::Rect2D {
                offset: vk::Offset2D {
                    x: region.dst_offset.x,
                    y: region.dst_offset.y,
                },
                extent,
            },
            (false, false),
            evidence,
        );
    }

    pub fn command_transferred(
        &self,
        command: vk::CommandBuffer,
        source: vk::Image,
        source_layers: vk::ImageSubresourceLayers,
        layout: vk::ImageLayout,
        destination: vk::Image,
        destination_layers: vk::ImageSubresourceLayers,
        source_rect: vk::Rect2D,
        destination_rect: vk::Rect2D,
        orientation: (bool, bool),
        evidence: Evidence,
    ) {
        if !source_layers
            .aspect_mask
            .contains(vk::ImageAspectFlags::COLOR)
            || !destination_layers
                .aspect_mask
                .contains(vk::ImageAspectFlags::COLOR)
        {
            return;
        }
        let rect = |value: vk::Rect2D| Rect {
            x: value.offset.x,
            y: value.offset.y,
            extent: Extent2D {
                width: value.extent.width,
                height: value.extent.height,
            },
        };
        for layer in 0..source_layers
            .layer_count
            .min(destination_layers.layer_count)
        {
            let source = self.surface_id(
                source,
                source_layers.mip_level,
                source_layers.base_array_layer + layer,
            );
            let destination = self.surface_id(
                destination,
                destination_layers.mip_level,
                destination_layers.base_array_layer + layer,
            );
            let mut edge = DependencyEdge::sampled(source, destination, 0);
            edge.evidence = evidence;
            edge.mapping = Some(RegionMapping {
                source: rect(source_rect),
                destination: rect(destination_rect),
                flip_x: orientation.0,
                flip_y: orientation.1,
            });
            edge.coverage = Some(rect(destination_rect));
            self.command_observations
                .entry(command)
                .or_default()
                .push(commands::RecordedCommand::SurfaceTransfer { edge, layout });
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use ash::vk::Handle;

    #[test]
    fn destruction_removes_target_only_observations_without_removing_siblings() {
        let tracking = Tracking::default();
        for raw in [1, 2] {
            let image = vk::Image::from_raw(raw);
            tracking.note_image(
                image,
                vk::Extent2D {
                    width: 64,
                    height: 64,
                },
            );
            tracking.surfaces.observations.target(
                polymer_presentation::Target {
                    id: tracking.surface_id(image, 0, 0),
                    score: 4096,
                    order: raw,
                },
                0,
            );
        }
        tracking.forget_image(vk::Image::from_raw(1));
        let (targets, edges) = tracking
            .surfaces
            .observations
            .graph::<()>(0)
            .observation_list();
        assert!(edges.is_empty());
        assert_eq!(targets.len(), 1);
        assert_eq!(targets[0].id.image, vk::Image::from_raw(2));
    }

    #[test]
    fn array_layers_and_mips_are_distinct_and_handle_reuse_invalidates_history() {
        let tracking = Tracking::default();
        let source = vk::Image::from_raw(1);
        let destination = vk::Image::from_raw(2);
        let command = vk::CommandBuffer::from_raw(3);
        tracking.note_image(
            source,
            vk::Extent2D {
                width: 128,
                height: 128,
            },
        );
        tracking.image_subresources(source, 2, 2);
        assert!(
            tracking
                .surface_extent(tracking.surface_id(source, 1, 1))
                .is_some()
        );
        assert!(
            tracking
                .surface_extent(tracking.surface_id(source, 2, 0))
                .is_none()
        );
        assert!(
            tracking
                .surface_extent(tracking.surface_id(source, 0, 2))
                .is_none()
        );
        tracking.swapchain_image_added(
            destination,
            vk::Extent2D {
                width: 128,
                height: 64,
            },
        );
        let extent = vk::Extent2D {
            width: 64,
            height: 64,
        };
        for layer in 0..2 {
            tracking.command_transferred(
                command,
                source,
                vk::ImageSubresourceLayers {
                    aspect_mask: vk::ImageAspectFlags::COLOR,
                    mip_level: 1,
                    base_array_layer: layer,
                    layer_count: 1,
                },
                vk::ImageLayout::TRANSFER_SRC_OPTIMAL,
                destination,
                vk::ImageSubresourceLayers {
                    aspect_mask: vk::ImageAspectFlags::COLOR,
                    layer_count: 1,
                    ..Default::default()
                },
                vk::Rect2D {
                    offset: vk::Offset2D::default(),
                    extent,
                },
                vk::Rect2D {
                    offset: vk::Offset2D {
                        x: layer as i32 * 64,
                        y: 0,
                    },
                    extent,
                },
                (false, layer == 1),
                Evidence::Blit,
            );
        }
        assert!(tracking.select_presented(destination, 2).is_empty());
        tracking.commands_submitted(&[command]);
        let selected = tracking.select_presented(destination, 2);
        assert_eq!(selected.len(), 2);
        assert_eq!(
            selected
                .iter()
                .map(|source| source.subresource)
                .collect::<Vec<_>>(),
            [(1, 0), (1, 1)]
        );
        assert!(selected.iter().all(|source| (
            source.selection.source_rect.extent.width,
            source.selection.source_rect.extent.height
        ) == (extent.width, extent.height)));
        assert_eq!(tracking.refresh_sources(&selected).len(), 2);
        tracking.image_destroyed(source);
        tracking.note_image(
            source,
            vk::Extent2D {
                width: 128,
                height: 128,
            },
        );
        assert!(tracking.refresh_sources(&selected).is_empty());
        tracking.commands_submitted(&[command]);
        assert!(tracking.select_presented(destination, 2).is_empty());
    }

    #[test]
    fn draw_descriptor_contents_are_resolved_at_submission() {
        let tracking = Tracking::default();
        let destination = vk::Image::from_raw(1);
        let source = vk::Image::from_raw(2);
        let replacement = vk::Image::from_raw(3);
        let command = vk::CommandBuffer::from_raw(4);
        let framebuffer = vk::Framebuffer::from_raw(5);
        let set = vk::DescriptorSet::from_raw(6);
        let extent = vk::Extent2D {
            width: 640,
            height: 480,
        };
        for image in [source, replacement] {
            tracking.note_image(image, extent);
            tracking
                .framebuffer_images
                .insert(framebuffer, vec![tracking.surface_id(image, 0, 0)]);
            tracking.submit_surface_draw(&CommandTrackingState {
                framebuffer: Some(framebuffer),
                viewport: Some(vk::Viewport {
                    width: 640.0,
                    height: 480.0,
                    ..Default::default()
                }),
                ..Default::default()
            });
        }
        tracking.swapchain_image_added(destination, extent);
        tracking
            .framebuffer_images
            .insert(framebuffer, vec![tracking.surface_id(destination, 0, 0)]);
        tracking.descriptor_set_images.insert(
            set,
            [(
                (0, 0),
                (
                    vec![tracking.surface_id(source, 0, 0)],
                    vk::ImageLayout::SHADER_READ_ONLY_OPTIMAL,
                ),
            )]
            .into(),
        );
        tracking.command_tracking.insert(
            command,
            CommandTrackingState {
                framebuffer: Some(framebuffer),
                descriptor_sets: vec![set],
                viewport: Some(vk::Viewport {
                    width: 640.0,
                    height: 480.0,
                    ..Default::default()
                }),
                dirty: true,
                ..Default::default()
            },
        );
        tracking.command_drew(command);
        tracking.descriptor_set_images.insert(
            set,
            [(
                (0, 0),
                (
                    vec![tracking.surface_id(replacement, 0, 0)],
                    vk::ImageLayout::SHADER_READ_ONLY_OPTIMAL,
                ),
            )]
            .into(),
        );
        tracking.commands_submitted(&[command]);
        let selected = tracking.select_presented(destination, 1);
        assert_eq!(selected.len(), 1);
        assert_eq!(selected[0].image, replacement);
        assert!(selected[0].selection.presentation_rect.is_none());
    }
}
impl Tracking {
    pub fn command_image_blitted(
        &self,
        command_buffer: vk::CommandBuffer,
        source_image: vk::Image,
        source_layout: vk::ImageLayout,
        destination_image: vk::Image,
        region: &vk::ImageBlit,
    ) {
        self.command_transferred(
            command_buffer,
            source_image,
            region.src_subresource,
            source_layout,
            destination_image,
            region.dst_subresource,
            vk::Rect2D {
                offset: vk::Offset2D {
                    x: region.src_offsets[0].x.min(region.src_offsets[1].x),
                    y: region.src_offsets[0].y.min(region.src_offsets[1].y),
                },
                extent: vk::Extent2D {
                    width: region.src_offsets[1].x.abs_diff(region.src_offsets[0].x),
                    height: region.src_offsets[1].y.abs_diff(region.src_offsets[0].y),
                },
            },
            vk::Rect2D {
                offset: vk::Offset2D {
                    x: region.dst_offsets[0].x.min(region.dst_offsets[1].x),
                    y: region.dst_offsets[0].y.min(region.dst_offsets[1].y),
                },
                extent: vk::Extent2D {
                    width: region.dst_offsets[1].x.abs_diff(region.dst_offsets[0].x),
                    height: region.dst_offsets[1].y.abs_diff(region.dst_offsets[0].y),
                },
            },
            (
                (region.src_offsets[0].x > region.src_offsets[1].x)
                    != (region.dst_offsets[0].x > region.dst_offsets[1].x),
                (region.src_offsets[0].y > region.src_offsets[1].y)
                    != (region.dst_offsets[0].y > region.dst_offsets[1].y),
            ),
            Evidence::Blit,
        );
    }
}
