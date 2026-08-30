use super::math::Vector3;
use eframe::egui;
use gltf::{image::Format, mesh::Mode};

const MODEL_BYTES: &[u8] = include_bytes!("../../../ngc.glb");

pub(super) struct ModelVertex {
    pub position: Vector3,
    pub normal: Vector3,
    pub texture_coordinate: egui::Pos2,
}

pub(super) struct ModelAsset {
    pub vertices: Vec<ModelVertex>,
    pub triangles: Vec<[u32; 3]>,
    pub texture: egui::ColorImage,
}

impl ModelAsset {
    pub fn load() -> Result<Self, String> {
        let (document, buffers, images) = gltf::import_slice(MODEL_BYTES)
            .map_err(|error| format!("could not parse ngc.glb: {error}"))?;
        let mut vertices = Vec::new();
        let mut triangles = Vec::new();
        for mesh in document.meshes() {
            for primitive in mesh.primitives() {
                if primitive.mode() != Mode::Triangles {
                    return Err("ngc.glb contains a non-triangle primitive".into());
                }
                let reader = primitive.reader(|buffer| Some(&buffers[buffer.index()].0));
                let positions = reader
                    .read_positions()
                    .ok_or("ngc.glb primitive has no positions")?;
                let normals = reader
                    .read_normals()
                    .ok_or("ngc.glb primitive has no normals")?;
                let texture_coordinates = reader
                    .read_tex_coords(0)
                    .ok_or("ngc.glb primitive has no texture coordinates")?
                    .into_f32();
                let first_vertex =
                    u32::try_from(vertices.len()).map_err(|_| "ngc.glb has too many vertices")?;
                vertices.extend(positions.zip(normals).zip(texture_coordinates).map(
                    |((position, normal), texture_coordinate)| ModelVertex {
                        position: Vector3::from_array(position),
                        normal: Vector3::from_array(normal),
                        texture_coordinate: egui::pos2(
                            texture_coordinate[0],
                            texture_coordinate[1],
                        ),
                    },
                ));
                let indices = reader
                    .read_indices()
                    .ok_or("ngc.glb primitive has no indices")?
                    .into_u32()
                    .collect::<Vec<_>>();
                if !indices.len().is_multiple_of(3) {
                    return Err("ngc.glb triangle index count is not divisible by three".into());
                }
                triangles.extend(indices.chunks_exact(3).map(|triangle| {
                    [
                        first_vertex + triangle[0],
                        first_vertex + triangle[1],
                        first_vertex + triangle[2],
                    ]
                }));
            }
        }
        if vertices.is_empty() || triangles.is_empty() {
            return Err("ngc.glb contains no renderable geometry".into());
        }
        Self::center_and_scale(&mut vertices);
        let texture = Self::base_color_texture(&images)?;
        Ok(Self {
            vertices,
            triangles,
            texture,
        })
    }

    fn center_and_scale(vertices: &mut [ModelVertex]) {
        let mut minimum = Vector3::new(f32::MAX, f32::MAX, f32::MAX);
        let mut maximum = Vector3::new(f32::MIN, f32::MIN, f32::MIN);
        for vertex in vertices.iter() {
            minimum.x = minimum.x.min(vertex.position.x);
            minimum.y = minimum.y.min(vertex.position.y);
            minimum.z = minimum.z.min(vertex.position.z);
            maximum.x = maximum.x.max(vertex.position.x);
            maximum.y = maximum.y.max(vertex.position.y);
            maximum.z = maximum.z.max(vertex.position.z);
        }
        let center = Vector3::new(
            (minimum.x + maximum.x) * 0.5,
            (minimum.y + maximum.y) * 0.5,
            (minimum.z + maximum.z) * 0.5,
        );
        let extent = (maximum.x - minimum.x)
            .max(maximum.y - minimum.y)
            .max(maximum.z - minimum.z);
        let scale = 4.8 / extent;
        for vertex in vertices {
            vertex.position = Vector3::new(
                (vertex.position.x - center.x) * scale,
                (vertex.position.y - center.y) * scale,
                (vertex.position.z - center.z) * scale,
            );
        }
    }

    fn base_color_texture(images: &[gltf::image::Data]) -> Result<egui::ColorImage, String> {
        let image = images.first().ok_or("ngc.glb has no base-color image")?;
        let rgba = match image.format {
            Format::R8G8B8 => image
                .pixels
                .chunks_exact(3)
                .flat_map(|pixel| [pixel[0], pixel[1], pixel[2], u8::MAX])
                .collect::<Vec<_>>(),
            Format::R8G8B8A8 => image.pixels.clone(),
            format => return Err(format!("unsupported ngc.glb base-color format: {format:?}")),
        };
        Ok(egui::ColorImage::from_rgba_unmultiplied(
            [image.width as usize, image.height as usize],
            &rgba,
        ))
    }
}
