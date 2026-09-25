use crate::webp::AnimatedWebp;
use std::time::Instant;

/// GPU resources for one animated WebP placeholder. Every frame lives in one
/// array texture; playback copies the current layer into the output.
pub(super) struct WebpTarget {
    frames: wgpu::Texture,
    durations: Vec<u32>,
    loop_ms: u64,
    shown: Option<u32>,
    pub(super) output: wgpu::Texture,
    started: Instant,
}

impl WebpTarget {
    pub(super) fn new(device: &wgpu::Device, queue: &wgpu::Queue, image: &AnimatedWebp) -> Self {
        let layer = wgpu::Extent3d {
            width: image.width,
            height: image.height,
            depth_or_array_layers: 1,
        };
        let texture = |label, layers, usage| {
            device.create_texture(&wgpu::TextureDescriptor {
                label: Some(label),
                size: wgpu::Extent3d {
                    depth_or_array_layers: layers,
                    ..layer
                },
                mip_level_count: 1,
                sample_count: 1,
                dimension: wgpu::TextureDimension::D2,
                format: wgpu::TextureFormat::Rgba8Unorm,
                usage,
                view_formats: &[],
            })
        };
        let frames = texture(
            "polymer webp frames",
            image.frames.len() as u32,
            wgpu::TextureUsages::COPY_DST | wgpu::TextureUsages::COPY_SRC,
        );
        for (index, frame) in image.frames.iter().enumerate() {
            let premultiplied = frame
                .rgba
                .chunks_exact(4)
                .flat_map(|pixel| {
                    let alpha = u16::from(pixel[3]);
                    let scale = |channel: u8| ((u16::from(channel) * alpha + 127) / 255) as u8;
                    [scale(pixel[0]), scale(pixel[1]), scale(pixel[2]), pixel[3]]
                })
                .collect::<Vec<_>>();
            queue.write_texture(
                wgpu::TexelCopyTextureInfo {
                    origin: wgpu::Origin3d {
                        z: index as u32,
                        ..wgpu::Origin3d::ZERO
                    },
                    ..frames.as_image_copy()
                },
                &premultiplied,
                wgpu::TexelCopyBufferLayout {
                    offset: 0,
                    bytes_per_row: Some(image.width * 4),
                    rows_per_image: Some(image.height),
                },
                layer,
            );
        }
        let durations = image
            .frames
            .iter()
            .map(|frame| frame.duration_ms)
            .collect::<Vec<_>>();
        Self {
            output: texture(
                "polymer webp output",
                1,
                wgpu::TextureUsages::COPY_DST
                    | wgpu::TextureUsages::COPY_SRC
                    | wgpu::TextureUsages::TEXTURE_BINDING,
            ),
            loop_ms: durations.iter().copied().map(u64::from).sum(),
            durations,
            shown: None,
            frames,
            started: Instant::now(),
        }
    }

    fn current(&self) -> u32 {
        let mut elapsed = self.started.elapsed().as_millis() as u64 % self.loop_ms.max(1);
        for (index, duration) in self.durations.iter().enumerate() {
            if elapsed < u64::from(*duration) {
                return index as u32;
            }
            elapsed -= u64::from(*duration);
        }
        0
    }

    pub(super) fn draw(&mut self, device: &wgpu::Device, queue: &wgpu::Queue) {
        let frame = self.current();
        if self.shown == Some(frame) {
            return;
        }
        self.shown = Some(frame);
        let mut encoder = device.create_command_encoder(&wgpu::CommandEncoderDescriptor {
            label: Some("polymer webp frame"),
        });
        encoder.copy_texture_to_texture(
            wgpu::TexelCopyTextureInfo {
                origin: wgpu::Origin3d {
                    z: frame,
                    ..wgpu::Origin3d::ZERO
                },
                ..self.frames.as_image_copy()
            },
            self.output.as_image_copy(),
            wgpu::Extent3d {
                depth_or_array_layers: 1,
                ..self.output.size()
            },
        );
        queue.submit([encoder.finish()]);
    }
}
