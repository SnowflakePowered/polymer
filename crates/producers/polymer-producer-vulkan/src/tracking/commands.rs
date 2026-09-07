use super::*;
#[derive(Clone)]
pub(super) enum RecordedCommand {
    SurfaceDraw(CommandTrackingState),
    SurfaceTransfer {
        edge: polymer_presentation::DependencyEdge<surfaces::SurfaceId>,
        layout: vk::ImageLayout,
    },
}

#[cfg(test)]
mod tests {
    use super::*;
    use ash::vk::Handle;

    #[test]
    fn submission_order_distinguishes_commands_and_refreshes_replay_within_one_generation() {
        let tracking = Tracking::default();
        let images = [1, 2, 3].map(vk::Image::from_raw);
        for image in images {
            tracking.note_image(
                image,
                vk::Extent2D {
                    width: 64,
                    height: 64,
                },
            );
        }
        let commands = [10, 11].map(vk::CommandBuffer::from_raw);
        for (index, command) in commands.iter().enumerate() {
            tracking.command_observations.insert(
                *command,
                vec![RecordedCommand::SurfaceTransfer {
                    edge: polymer_presentation::DependencyEdge::sampled(
                        tracking.surface_id(images[index], 0, 0),
                        tracking.surface_id(images[2], 0, 0),
                        0,
                    ),
                    layout: vk::ImageLayout::GENERAL,
                }],
            );
        }
        assert!(
            tracking
                .surfaces
                .observations
                .graph::<()>(0)
                .observation_list()
                .1
                .is_empty()
        );
        tracking.commands_submitted(&commands);
        let first = tracking
            .surfaces
            .observations
            .graph::<()>(0)
            .observation_list()
            .1;
        assert_eq!(first.len(), 2);
        assert!(first[0].order < first[1].order);
        tracking.commands_submitted(&[commands[1], commands[0]]);
        let replay = tracking
            .surfaces
            .observations
            .graph::<()>(0)
            .observation_list()
            .1;
        assert_eq!(replay.len(), 2);
        assert!(
            replay
                .iter()
                .all(|edge| edge.generation == 0 && edge.order > first[1].order)
        );
        assert!(replay[1].order < replay[0].order);
        tracking.command_buffer_began(commands[0]);
        tracking.commands_submitted(&[commands[0]]);
        assert_eq!(
            tracking
                .surfaces
                .observations
                .graph::<()>(0)
                .observation_list()
                .1,
            replay
        );
    }

    #[test]
    fn copy_and_resolve_batches_preserve_distinct_evidence_and_subresource_crop() {
        let tracking = Tracking::default();
        let command = vk::CommandBuffer::from_raw(1);
        let source = vk::Image::from_raw(2);
        let destination = vk::Image::from_raw(3);
        tracking.note_image(
            source,
            vk::Extent2D {
                width: 128,
                height: 64,
            },
        );
        tracking.image_subresources(source, 2, 2);
        tracking.swapchain_image_added(
            destination,
            vk::Extent2D {
                width: 320,
                height: 240,
            },
        );
        let region = vk::ImageCopy {
            src_subresource: vk::ImageSubresourceLayers {
                aspect_mask: vk::ImageAspectFlags::COLOR,
                mip_level: 0,
                base_array_layer: 1,
                layer_count: 1,
            },
            src_offset: vk::Offset3D { x: 8, y: 4, z: 0 },
            dst_subresource: vk::ImageSubresourceLayers {
                aspect_mask: vk::ImageAspectFlags::COLOR,
                layer_count: 1,
                ..Default::default()
            },
            dst_offset: vk::Offset3D { x: 96, y: 40, z: 0 },
            extent: vk::Extent3D {
                width: 32,
                height: 16,
                depth: 1,
            },
        };
        for evidence in [
            polymer_presentation::Evidence::Copy,
            polymer_presentation::Evidence::Resolve,
        ] {
            tracking.command_image_transferred(
                command,
                source,
                vk::ImageLayout::TRANSFER_SRC_OPTIMAL,
                destination,
                &region,
                evidence,
            );
        }
        assert!(
            tracking
                .surfaces
                .observations
                .graph::<()>(0)
                .observation_list()
                .1
                .is_empty()
        );
        tracking.commands_submitted(&[command]);
        let edges = tracking
            .surfaces
            .observations
            .graph::<()>(0)
            .observation_list()
            .1;
        assert_eq!(edges.len(), 2);
        assert!(
            edges
                .iter()
                .any(|edge| edge.evidence == polymer_presentation::Evidence::Copy)
        );
        assert!(
            edges
                .iter()
                .any(|edge| edge.evidence == polymer_presentation::Evidence::Resolve)
        );
        for edge in edges {
            assert_eq!((edge.source.mip, edge.source.layer), (0, 1));
            let mapping = edge.mapping.unwrap();
            assert_eq!(
                (
                    mapping.source.x,
                    mapping.source.y,
                    mapping.source.extent.width
                ),
                (8, 4, 32)
            );
            assert_eq!((mapping.destination.x, mapping.destination.y), (96, 40));
            assert!(!edge.whole_source);
        }
    }

    #[test]
    fn transfer_batches_require_submission_and_support_secondary_replay_and_reset() {
        let tracking = Tracking::default();
        let source = vk::Image::from_raw(1);
        let destination = vk::Image::from_raw(2);
        let primary = vk::CommandBuffer::from_raw(3);
        let secondary = vk::CommandBuffer::from_raw(4);
        let extent = vk::Extent2D {
            width: 640,
            height: 480,
        };
        tracking.note_image(source, extent);
        tracking.swapchain_image_added(destination, extent);
        tracking.command_blitted(
            secondary,
            source,
            vk::ImageLayout::TRANSFER_SRC_OPTIMAL,
            destination,
            vk::Offset2D::default(),
            extent,
            vk::Offset2D::default(),
            extent,
            (true, false),
        );
        assert!(
            tracking
                .surfaces
                .observations
                .graph::<()>(0)
                .observation_list()
                .1
                .is_empty()
        );
        tracking.command_executed(primary, &[secondary]);
        tracking.commands_submitted(&[primary]);
        let edges = tracking
            .surfaces
            .observations
            .graph::<()>(0)
            .observation_list()
            .1;
        assert!(edges[0].exact());
        assert_eq!(edges[0].mapping.unwrap().flip_x, true);
        tracking.surfaces.observations.clear();
        tracking.commands_submitted(&[primary]);
        assert!(
            !tracking
                .surfaces
                .observations
                .graph::<()>(0)
                .observation_list()
                .1
                .is_empty()
        );
        // Resetting a referenced secondary also invalidates its primary batch.
        tracking.command_buffer_began(secondary);
        tracking.surfaces.observations.clear();
        tracking.commands_submitted(&[primary, secondary]);
        assert!(
            tracking
                .surfaces
                .observations
                .graph::<()>(0)
                .observation_list()
                .1
                .is_empty()
        );
    }
    #[test]
    fn pool_reset_free_and_destroy_invalidate_dependents_but_preserve_other_pools() {
        for operation in 0..3 {
            let tracking = Tracking::default();
            let secondary_pool = vk::CommandPool::from_raw(10);
            let primary_pool = vk::CommandPool::from_raw(11);
            let primary = vk::CommandBuffer::from_raw(20);
            let secondary = vk::CommandBuffer::from_raw(21);
            let independent = vk::CommandBuffer::from_raw(22);
            let destination = vk::Image::from_raw(30);
            let extent = vk::Extent2D {
                width: 640,
                height: 480,
            };
            tracking.commands_allocated(secondary_pool, &[secondary]);
            tracking.commands_allocated(primary_pool, &[primary, independent]);
            tracking.swapchain_image_added(destination, extent);
            for (command, source) in [(secondary, 31), (independent, 32)] {
                let source = vk::Image::from_raw(source);
                tracking.note_image(source, extent);
                tracking.command_blitted(
                    command,
                    source,
                    vk::ImageLayout::TRANSFER_SRC_OPTIMAL,
                    destination,
                    vk::Offset2D::default(),
                    extent,
                    vk::Offset2D::default(),
                    extent,
                    (false, false),
                );
            }
            tracking.command_executed(primary, &[secondary]);
            tracking.commands_submitted(&[primary, independent]);
            assert_eq!(
                tracking
                    .surfaces
                    .observations
                    .graph::<()>(0)
                    .observation_list()
                    .1
                    .len(),
                2
            );
            match operation {
                0 => tracking.command_pool_reset(secondary_pool, false),
                1 => tracking.commands_freed(&[secondary]),
                _ => tracking.command_pool_reset(secondary_pool, true),
            }
            tracking.surfaces.observations.clear();
            tracking.commands_submitted(&[primary, secondary, independent]);
            let edges = tracking
                .surfaces
                .observations
                .graph::<()>(0)
                .observation_list()
                .1;
            assert_eq!(edges.len(), 1);
            assert_eq!(edges[0].source.image, vk::Image::from_raw(32));
            // A recycled command-buffer handle cannot replay the freed batch.
            tracking.commands_allocated(secondary_pool, &[secondary]);
            tracking.surfaces.observations.clear();
            tracking.command_executed(primary, &[secondary]);
            tracking.commands_submitted(&[primary, secondary]);
            assert!(
                tracking
                    .surfaces
                    .observations
                    .graph::<()>(0)
                    .observation_list()
                    .1
                    .is_empty()
            );
            assert_eq!(
                *tracking.command_pools.get(&independent).unwrap(),
                primary_pool
            );
        }
    }
}
impl Tracking {
    pub fn commands_allocated(&self, pool: vk::CommandPool, commands: &[vk::CommandBuffer]) {
        for &command in commands {
            self.command_buffer_began(command);
            self.command_pools.insert(command, pool);
        }
    }

    pub fn commands_freed(&self, commands: &[vk::CommandBuffer]) {
        for &command in commands {
            self.command_buffer_began(command);
            self.command_pools.remove(&command);
        }
    }

    pub fn command_pool_reset(&self, pool: vk::CommandPool, destroyed: bool) {
        let commands = self
            .command_pools
            .iter()
            .filter(|entry| *entry.value() == pool)
            .map(|entry| *entry.key())
            .collect::<Vec<_>>();
        for &command in &commands {
            self.command_buffer_began(command);
        }
        if destroyed {
            self.commands_freed(&commands);
        }
    }

    pub fn command_executed(&self, primary: vk::CommandBuffer, secondary: &[vk::CommandBuffer]) {
        self.command_dependencies
            .entry(primary)
            .or_default()
            .extend(
                secondary
                    .iter()
                    .copied()
                    .filter(|command| *command != primary),
            );
        let inherited = self
            .command_tracking
            .get(&primary)
            .and_then(|state| state.framebuffer);
        let observations = secondary
            .iter()
            .filter(|&&command| command != primary)
            .flat_map(|command| {
                self.command_observations
                    .get(command)
                    .map_or_else(Vec::new, |batch| batch.clone())
            })
            .map(|mut observation| {
                if let RecordedCommand::SurfaceDraw(state) = &mut observation {
                    state.framebuffer = state.framebuffer.or(inherited);
                }
                observation
            })
            .collect::<Vec<_>>();
        self.command_observations
            .entry(primary)
            .or_default()
            .extend(observations);
    }

    #[cfg(test)]
    pub fn command_blitted(
        &self,
        command: vk::CommandBuffer,
        source: vk::Image,
        layout: vk::ImageLayout,
        destination: vk::Image,
        source_offset: vk::Offset2D,
        source_extent: vk::Extent2D,
        destination_offset: vk::Offset2D,
        destination_extent: vk::Extent2D,
        orientation: (bool, bool),
    ) {
        self.command_transferred(
            command,
            source,
            vk::ImageSubresourceLayers {
                aspect_mask: vk::ImageAspectFlags::COLOR,
                layer_count: 1,
                ..Default::default()
            },
            layout,
            destination,
            vk::ImageSubresourceLayers {
                aspect_mask: vk::ImageAspectFlags::COLOR,
                layer_count: 1,
                ..Default::default()
            },
            vk::Rect2D {
                offset: source_offset,
                extent: source_extent,
            },
            vk::Rect2D {
                offset: destination_offset,
                extent: destination_extent,
            },
            orientation,
            polymer_presentation::Evidence::Blit,
        );
    }
    /// Called only after queue submission succeeds. Re-execution refreshes every observation.
    pub fn commands_submitted(&self, commands: &[vk::CommandBuffer]) {
        let _observations = self.surfaces.observations.transaction();
        for &command in commands {
            let Some(batch) = self.command_observations.get(&command) else {
                continue;
            };
            for observation in batch.iter() {
                self.submit_observation(command, observation);
            }
        }
    }
    fn submit_observation(&self, _command: vk::CommandBuffer, observation: &RecordedCommand) {
        match observation {
            RecordedCommand::SurfaceDraw(state) => self.submit_surface_draw(state),
            RecordedCommand::SurfaceTransfer { edge, layout } => {
                let order = self.surfaces.order.fetch_add(1, Ordering::Relaxed);
                self.submit_surface_edge(*edge, *layout, order);
            }
        }
    }
}
