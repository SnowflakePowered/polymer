use crate::ps2::{Animation, Lighting, Ps2Icon, TEXTURE_SIZE};
use bytemuck::{Pod, Zeroable};

#[repr(C)]
#[derive(Clone, Copy, Pod, Zeroable)]
pub(super) struct Vertex {
    position: [f32; 3],
    normal: [f32; 3],
    uv: [f32; 2],
    color: [f32; 4],
}

pub(super) struct Texture {
    pub(super) width: u32,
    pub(super) height: u32,
    pub(super) rgba: Vec<u8>,
}

pub(super) struct Mesh {
    shapes: Vec<Vec<Vertex>>,
    animation: Animation,
    pub(super) lighting: Lighting,
    pub(super) texture: Texture,
    pub(super) min: [f32; 3],
    pub(super) max: [f32; 3],
}

impl Mesh {
    pub(super) fn new(icon: &Ps2Icon) -> Self {
        let shapes = (0..icon.shapes)
            .map(|shape| {
                icon.vertices
                    .iter()
                    .enumerate()
                    .map(|(index, vertex)| Vertex {
                        position: icon.position(index, shape),
                        normal: vertex.normal.to_f32(),
                        uv: vertex.texture.uv(),
                        color: vertex.texture.color(),
                    })
                    .collect::<Vec<_>>()
            })
            .collect::<Vec<_>>();
        let mut min = [f32::INFINITY; 3];
        let mut max = [f32::NEG_INFINITY; 3];
        for vertex in shapes.iter().flatten() {
            for axis in 0..3 {
                min[axis] = min[axis].min(vertex.position[axis]);
                max[axis] = max[axis].max(vertex.position[axis]);
            }
        }
        let texture = match &icon.texture {
            Some(rgba) => Texture {
                width: TEXTURE_SIZE,
                height: TEXTURE_SIZE,
                rgba: rgba.clone(),
            },
            None => Texture {
                width: 1,
                height: 1,
                rgba: vec![u8::MAX; 4],
            },
        };
        Self {
            shapes,
            animation: icon.animation.clone(),
            lighting: icon.lighting,
            texture,
            min,
            max,
        }
    }

    pub(super) fn vertex_count(&self) -> usize {
        self.shapes[0].len()
    }

    /// The mesh `time` seconds into its animation, blending every shape by
    /// its keyframed weight.
    pub(super) fn pose(&self, time: f32, output: &mut Vec<Vertex>) {
        let mut weights = vec![0.0; self.shapes.len()];
        self.animation.weights(time, &mut weights);
        output.clone_from(&self.shapes[0]);
        output
            .iter_mut()
            .for_each(|vertex| vertex.position = [0.0; 3]);
        for (shape, weight) in self
            .shapes
            .iter()
            .zip(weights)
            .filter(|(_, weight)| *weight != 0.0)
        {
            for (vertex, source) in output.iter_mut().zip(shape) {
                for axis in 0..3 {
                    vertex.position[axis] += source.position[axis] * weight;
                }
            }
        }
    }
}
