use std::{borrow::Cow, io::Cursor};

use gltf::{Glb, Gltf, binary::Header};
use image::{DynamicImage, ImageFormat, RgbImage};
use serde_json::{Value, json};
use zerocopy::{
    FromBytes, Immutable, KnownLayout, Unaligned,
    byteorder::little_endian::{I16, U32},
};

use super::{IconSave, SaveIcon};
use crate::impls::ps2::IconSaveContainer;

const ARRAY_BUFFER: u32 = 34_962;
const FLOAT: u32 = 5_126;
const TEXTURE_SIZE: u32 = 128;
const TEXTURE_PIXELS: usize = TEXTURE_SIZE as usize * TEXTURE_SIZE as usize;
const RAW_TEXTURE_SIZE: usize = TEXTURE_PIXELS * 2;
const FIXED_POINT_SCALE: f32 = 4096.0;
const PS2_COLOR_SCALE: f32 = 128.0;

// On-disk layouts documented by ps2iconsys:
// https://github.com/ticky/ps2iconsys/blob/develop/include/ps2_ps2icon.hpp
#[derive(FromBytes, Immutable, KnownLayout, Unaligned)]
#[repr(C)]
struct IconSys {
    magic: [u8; 4],
    before_normal_icon: [u8; 256],
    normal_icon: [u8; 64],
    copy_icon: [u8; 64],
    delete_icon: [u8; 64],
    reserved: [u8; 512],
}

#[derive(FromBytes, Immutable, KnownLayout, Unaligned)]
#[repr(C)]
struct IconHeader {
    file_id: U32,
    animation_shapes: U32,
    texture_type: U32,
    reserved: U32,
    vertex_count: U32,
}

#[derive(Clone, Copy, FromBytes, Immutable, KnownLayout, Unaligned)]
#[repr(C)]
struct VertexCoord {
    x: I16,
    y: I16,
    z: I16,
    unknown: I16,
}

#[derive(Clone, Copy, FromBytes, Immutable, KnownLayout, Unaligned)]
#[repr(C)]
struct TextureData {
    u: I16,
    v: I16,
    color: [u8; 4],
}

#[derive(FromBytes, Immutable, KnownLayout, Unaligned)]
#[repr(C)]
struct AnimationHeader {
    id_tag: U32,
    frame_length: U32,
    animation_speed: [u8; 4],
    play_offset: U32,
    frame_count: U32,
}

#[derive(FromBytes, Immutable, KnownLayout, Unaligned)]
#[repr(C)]
struct FrameData {
    shape_id: U32,
    key_count: U32,
}

#[derive(FromBytes, Immutable, KnownLayout, Unaligned)]
#[repr(C)]
struct FrameKey {
    time: [u8; 4],
    value: [u8; 4],
}

#[derive(Clone, Copy)]
struct Vertex {
    normal: VertexCoord,
    texture: TextureData,
}

struct Icon {
    animation_shapes: usize,
    vertex_count: usize,
    coords: Vec<VertexCoord>,
    vertices: Vec<Vertex>,
    frame_length: u32,
    animation_speed: f32,
    texture_rgb: Option<Vec<u8>>,
}

const _: () = assert!(size_of::<IconSys>() == 964);
const _: () = assert!(size_of::<IconHeader>() == 20);
const _: () = assert!(size_of::<VertexCoord>() == 8);
const _: () = assert!(size_of::<TextureData>() == 8);
const _: () = assert!(size_of::<AnimationHeader>() == 20);
const _: () = assert!(size_of::<FrameData>() == 8);
const _: () = assert!(size_of::<FrameKey>() == 8);

impl IconSave for IconSaveContainer<'_> {
    fn read_icon(&self) -> Option<SaveIcon> {
        struct GlbBuilder {
            bytes: Vec<u8>,
            buffer_views: Vec<Value>,
            accessors: Vec<Value>,
        }

        impl GlbBuilder {
            fn new() -> Self {
                Self {
                    bytes: Vec::new(),
                    buffer_views: Vec::new(),
                    accessors: Vec::new(),
                }
            }

            fn align(&mut self) {
                while !self.bytes.len().is_multiple_of(4) {
                    self.bytes.push(0);
                }
            }

            fn push_view(&mut self, bytes: &[u8], target: Option<u32>) -> usize {
                self.align();
                let byte_offset = self.bytes.len();
                self.bytes.extend_from_slice(bytes);
                let mut view = json!({
                    "buffer": 0,
                    "byteOffset": byte_offset,
                    "byteLength": bytes.len(),
                });
                if let Some(target) = target {
                    view["target"] = target.into();
                }
                let index = self.buffer_views.len();
                self.buffer_views.push(view);
                index
            }

            fn push_f32_accessor(
                &mut self,
                values: &[f32],
                element_type: &'static str,
                element_count: usize,
                min: Option<&[f32]>,
                max: Option<&[f32]>,
            ) -> usize {
                let mut bytes = Vec::with_capacity(std::mem::size_of_val(values));
                for value in values {
                    bytes.extend_from_slice(&value.to_le_bytes());
                }
                let view = self.push_view(&bytes, Some(ARRAY_BUFFER));
                let mut accessor = json!({
                    "bufferView": view,
                    "componentType": FLOAT,
                    "count": element_count,
                    "type": element_type,
                });
                if let Some(min) = min {
                    accessor["min"] = json!(min);
                }
                if let Some(max) = max {
                    accessor["max"] = json!(max);
                }
                let index = self.accessors.len();
                self.accessors.push(accessor);
                index
            }
        }

        fn read_model(bytes: &[u8]) -> Option<Gltf> {
            let icon = parse_icon(bytes)?;
            let glb = build(icon)?;
            Gltf::from_slice(&glb.to_vec().ok()?).ok()
        }

        fn take<'a, T>(bytes: &'a [u8], offset: &mut usize) -> Option<&'a T>
        where
            T: FromBytes + KnownLayout + Immutable,
        {
            let value = T::ref_from_prefix(bytes.get(*offset..)?).ok()?.0;
            *offset = offset.checked_add(size_of::<T>())?;
            Some(value)
        }

        fn parse_icon(bytes: &[u8]) -> Option<Icon> {
            let mut offset = 0;
            let header = take::<IconHeader>(bytes, &mut offset)?;
            let animation_shapes = usize::try_from(header.animation_shapes.get()).ok()?;
            let vertex_count = usize::try_from(header.vertex_count.get()).ok()?;
            if animation_shapes == 0 || vertex_count == 0 || !vertex_count.is_multiple_of(3) {
                return None;
            }
            let coordinate_count = vertex_count.checked_mul(animation_shapes)?;
            let vertex_bytes = coordinate_count
                .checked_mul(size_of::<VertexCoord>())?
                .checked_add(
                    vertex_count
                        .checked_mul(size_of::<VertexCoord>() + size_of::<TextureData>())?,
                )?;
            if bytes.len().saturating_sub(offset) < vertex_bytes + size_of::<AnimationHeader>() {
                return None;
            }

            let mut coords = Vec::with_capacity(coordinate_count);
            let mut vertices = Vec::with_capacity(vertex_count);
            for _ in 0..vertex_count {
                for _ in 0..animation_shapes {
                    coords.push(*take::<VertexCoord>(bytes, &mut offset)?);
                }
                vertices.push(Vertex {
                    normal: *take::<VertexCoord>(bytes, &mut offset)?,
                    texture: *take::<TextureData>(bytes, &mut offset)?,
                });
            }

            let animation = take::<AnimationHeader>(bytes, &mut offset)?;
            if animation.id_tag.get() != 1 {
                return None;
            }
            let frame_length = animation.frame_length.get();
            let animation_speed = f32::from_le_bytes(animation.animation_speed);
            let frame_count = usize::try_from(animation.frame_count.get()).ok()?;
            for _ in 0..frame_count {
                let frame = take::<FrameData>(bytes, &mut offset)?;
                if usize::try_from(frame.shape_id.get()).ok()? >= animation_shapes {
                    return None;
                }
                for _ in 0..frame.key_count.get() {
                    let _ = take::<FrameKey>(bytes, &mut offset)?;
                }
            }

            let texture_type = header.texture_type.get();
            let texture_rgb = if texture_type & 0b0100 == 0 {
                None
            } else {
                let raw = if texture_type & 0b1000 == 0 {
                    bytes
                        .get(offset..offset.checked_add(RAW_TEXTURE_SIZE)?)?
                        .to_vec()
                } else {
                    let size = usize::try_from(take::<U32>(bytes, &mut offset)?.get()).ok()?;
                    let compressed = bytes.get(offset..offset.checked_add(size)?)?;
                    decode_rle(compressed)?
                };
                Some(decode_texture(&raw)?)
            };

            Some(Icon {
                animation_shapes,
                vertex_count,
                coords,
                vertices,
                frame_length,
                animation_speed,
                texture_rgb,
            })
        }

        fn decode_rle(bytes: &[u8]) -> Option<Vec<u8>> {
            let mut input = 0;
            let mut output = Vec::with_capacity(RAW_TEXTURE_SIZE);
            while input < bytes.len() {
                let code = u16::from_le_bytes(bytes.get(input..input + 2)?.try_into().ok()?);
                input += 2;
                if code >= 0xff00 {
                    let pixels = usize::from(!code) + 1;
                    let length = pixels.checked_mul(2)?;
                    if output.len().checked_add(length)? > RAW_TEXTURE_SIZE {
                        return None;
                    }
                    output.extend_from_slice(bytes.get(input..input.checked_add(length)?)?);
                    input += length;
                } else if code != 0 {
                    let pixel: [u8; 2] = bytes.get(input..input + 2)?.try_into().ok()?;
                    input += 2;
                    let length = usize::from(code).checked_mul(2)?;
                    if output.len().checked_add(length)? > RAW_TEXTURE_SIZE {
                        return None;
                    }
                    for _ in 0..code {
                        output.extend_from_slice(&pixel);
                    }
                }
            }
            (output.len() == RAW_TEXTURE_SIZE).then_some(output)
        }

        fn decode_texture(bytes: &[u8]) -> Option<Vec<u8>> {
            if bytes.len() != RAW_TEXTURE_SIZE {
                return None;
            }
            let mut rgb = Vec::with_capacity(TEXTURE_PIXELS * 3);
            for pixel in bytes.chunks_exact(2) {
                let color = u16::from_le_bytes([pixel[0], pixel[1]]);
                let expand = |channel: u16| ((channel << 3) | (channel >> 2)) as u8;
                rgb.extend_from_slice(&[
                    expand(color & 0x1f),
                    expand((color >> 5) & 0x1f),
                    expand((color >> 10) & 0x1f),
                ]);
            }
            Some(rgb)
        }

        fn validate(icon: &Icon) -> Option<()> {
            if icon.animation_shapes == 0
                || icon.vertex_count == 0
                || !icon.vertex_count.is_multiple_of(3)
            {
                return None;
            }
            let expected_coords = icon.vertex_count.checked_mul(icon.animation_shapes)?;
            (icon.coords.len() == expected_coords && icon.vertices.len() == icon.vertex_count)
                .then_some(())
        }

        fn position(icon: &Icon, vertex: usize, shape: usize) -> [f32; 3] {
            let coord = icon.coords[vertex * icon.animation_shapes + shape];
            [
                f32::from(coord.x.get()) / FIXED_POINT_SCALE,
                -f32::from(coord.y.get()) / FIXED_POINT_SCALE,
                f32::from(coord.z.get()) / FIXED_POINT_SCALE,
            ]
        }

        fn positions(icon: &Icon, shape: usize) -> (Vec<f32>, [f32; 3], [f32; 3]) {
            let mut values = Vec::with_capacity(icon.vertex_count * 3);
            let mut min = [f32::INFINITY; 3];
            let mut max = [f32::NEG_INFINITY; 3];
            for vertex in 0..icon.vertex_count {
                let position = position(icon, vertex, shape);
                for axis in 0..3 {
                    min[axis] = min[axis].min(position[axis]);
                    max[axis] = max[axis].max(position[axis]);
                }
                values.extend_from_slice(&position);
            }
            (values, min, max)
        }

        fn normals(icon: &Icon) -> Vec<f32> {
            let mut values = Vec::with_capacity(icon.vertex_count * 3);
            for vertex in &icon.vertices {
                values.extend_from_slice(&[
                    f32::from(vertex.normal.x.get()) / FIXED_POINT_SCALE,
                    -f32::from(vertex.normal.y.get()) / FIXED_POINT_SCALE,
                    f32::from(vertex.normal.z.get()) / FIXED_POINT_SCALE,
                ]);
            }
            values
        }

        fn texture_coordinates(icon: &Icon) -> Vec<f32> {
            let mut values = Vec::with_capacity(icon.vertex_count * 2);
            for vertex in &icon.vertices {
                values.extend_from_slice(&[
                    f32::from(vertex.texture.u.get()) / FIXED_POINT_SCALE,
                    f32::from(vertex.texture.v.get()) / FIXED_POINT_SCALE,
                ]);
            }
            values
        }

        fn colors(icon: &Icon) -> Vec<f32> {
            let mut values = Vec::with_capacity(icon.vertex_count * 4);
            for vertex in &icon.vertices {
                values.extend_from_slice(&[
                    (f32::from(vertex.texture.color[0]) / PS2_COLOR_SCALE).min(1.0),
                    (f32::from(vertex.texture.color[1]) / PS2_COLOR_SCALE).min(1.0),
                    (f32::from(vertex.texture.color[2]) / PS2_COLOR_SCALE).min(1.0),
                    (f32::from(vertex.texture.color[3]) / PS2_COLOR_SCALE).min(1.0),
                ]);
            }
            values
        }

        fn encode_texture(texture: &[u8]) -> Option<Vec<u8>> {
            let image = RgbImage::from_raw(TEXTURE_SIZE, TEXTURE_SIZE, texture.to_vec())?;
            let mut output = Cursor::new(Vec::new());
            DynamicImage::ImageRgb8(image)
                .write_to(&mut output, ImageFormat::Png)
                .ok()?;
            Some(output.into_inner())
        }

        fn animation(icon: &Icon, builder: &mut GlbBuilder) -> Option<Value> {
            let target_count = icon.animation_shapes.checked_sub(1)?;
            if target_count == 0 {
                return None;
            }

            let duration = if icon.animation_speed.is_finite() && icon.animation_speed > 0.0 {
                icon.frame_length as f32 / (60.0 * icon.animation_speed)
            } else {
                icon.animation_shapes as f32 / 60.0
            }
            .max(f32::EPSILON);
            let keyframe_count = icon.animation_shapes + 1;
            let mut times = Vec::with_capacity(keyframe_count);
            let mut weights = Vec::with_capacity(keyframe_count * target_count);
            for keyframe in 0..keyframe_count {
                times.push(duration * keyframe as f32 / icon.animation_shapes as f32);
                for target in 0..target_count {
                    weights.push(
                        if keyframe > 0
                            && keyframe < icon.animation_shapes
                            && target == keyframe - 1
                        {
                            1.0
                        } else {
                            0.0
                        },
                    );
                }
            }

            let time_accessor = builder.push_f32_accessor(
                &times,
                "SCALAR",
                times.len(),
                Some(&[0.0]),
                Some(&[duration]),
            );
            let weight_accessor =
                builder.push_f32_accessor(&weights, "SCALAR", weights.len(), None, None);
            Some(json!({
                "name": "PS2 icon animation",
                "samplers": [{
                    "input": time_accessor,
                    "interpolation": "LINEAR",
                    "output": weight_accessor,
                }],
                "channels": [{
                    "sampler": 0,
                    "target": {
                        "node": 0,
                        "path": "weights",
                    },
                }],
            }))
        }

        fn build(icon: Icon) -> Option<Glb<'static>> {
            validate(&icon)?;
            let mut builder = GlbBuilder::new();

            let (positions, position_min, position_max) = positions(&icon, 0);
            let position_accessor = builder.push_f32_accessor(
                &positions,
                "VEC3",
                icon.vertex_count,
                Some(&position_min),
                Some(&position_max),
            );
            let normal_accessor =
                builder.push_f32_accessor(&normals(&icon), "VEC3", icon.vertex_count, None, None);
            let texcoord_accessor = builder.push_f32_accessor(
                &texture_coordinates(&icon),
                "VEC2",
                icon.vertex_count,
                None,
                None,
            );
            let color_accessor =
                builder.push_f32_accessor(&colors(&icon), "VEC4", icon.vertex_count, None, None);

            let mut targets = Vec::with_capacity(icon.animation_shapes.saturating_sub(1));
            for shape in 1..icon.animation_shapes {
                let mut deltas = Vec::with_capacity(icon.vertex_count * 3);
                for vertex in 0..icon.vertex_count {
                    let base = position(&icon, vertex, 0);
                    let next = position(&icon, vertex, shape);
                    deltas.extend_from_slice(&[
                        next[0] - base[0],
                        next[1] - base[1],
                        next[2] - base[2],
                    ]);
                }
                let accessor =
                    builder.push_f32_accessor(&deltas, "VEC3", icon.vertex_count, None, None);
                targets.push(json!({ "POSITION": accessor }));
            }

            let mut material = json!({
                "name": "PS2 icon material",
                "doubleSided": true,
                "pbrMetallicRoughness": {
                    "metallicFactor": 0.0,
                    "roughnessFactor": 1.0,
                },
            });
            if icon
                .vertices
                .iter()
                .any(|vertex| vertex.texture.color[3] < 0x80)
            {
                material["alphaMode"] = "BLEND".into();
            }

            let mut images = Vec::new();
            let mut textures = Vec::new();
            if let Some(texture) = icon.texture_rgb.as_deref() {
                let png = encode_texture(texture)?;
                let view = builder.push_view(&png, None);
                images.push(json!({
                    "name": "PS2 icon texture",
                    "bufferView": view,
                    "mimeType": "image/png",
                }));
                textures.push(json!({ "source": 0 }));
                material["pbrMetallicRoughness"]["baseColorTexture"] = json!({ "index": 0 });
            }

            let animation = animation(&icon, &mut builder);
            let mut primitive = json!({
                "attributes": {
                    "POSITION": position_accessor,
                    "NORMAL": normal_accessor,
                    "TEXCOORD_0": texcoord_accessor,
                    "COLOR_0": color_accessor,
                },
                "material": 0,
            });
            if !targets.is_empty() {
                primitive["targets"] = targets.into();
            }

            let mut root = json!({
                "asset": {
                    "version": "2.0",
                    "generator": "snowglass PS2 icon reader",
                },
                "scene": 0,
                "scenes": [{
                    "name": "PS2 memory card icon",
                    "nodes": [0],
                }],
                "nodes": [{
                    "name": "PS2 memory card icon",
                    "mesh": 0,
                }],
                "meshes": [{
                    "name": "PS2 memory card icon",
                    "primitives": [primitive],
                    "weights": vec![0.0; icon.animation_shapes.saturating_sub(1)],
                }],
                "materials": [material],
                "buffers": [{
                    "byteLength": builder.bytes.len(),
                }],
                "bufferViews": builder.buffer_views,
                "accessors": builder.accessors,
            });
            if !images.is_empty() {
                root["images"] = images.into();
                root["textures"] = textures.into();
            }
            if let Some(animation) = animation {
                root["animations"] = json!([animation]);
            }

            let json = serde_json::to_vec(&root).ok()?;
            let json_length = json.len().next_multiple_of(4);
            let bin_length = builder.bytes.len().next_multiple_of(4);
            let length = 12_usize
                .checked_add(8 + json_length)?
                .checked_add(8 + bin_length)?;
            let length = u32::try_from(length).ok()?;

            Some(Glb {
                header: Header {
                    magic: *b"glTF",
                    version: 2,
                    length,
                },
                json: Cow::Owned(json),
                bin: Some(Cow::Owned(builder.bytes)),
            })
        }

        let files = self.files().collect::<Vec<_>>();
        let icon_sys = files
            .iter()
            .find(|file| file.name_bytes().eq_ignore_ascii_case(b"icon.sys"))?;
        let icon_sys = IconSys::ref_from_bytes(icon_sys.data()).ok()?;
        if icon_sys.magic != *b"PS2D" {
            return None;
        }
        let name_end = icon_sys
            .normal_icon
            .iter()
            .position(|byte| *byte == 0)
            .unwrap_or(icon_sys.normal_icon.len());
        let icon_name = &icon_sys.normal_icon[..name_end];
        let icon = files
            .iter()
            .find(|file| file.name_bytes().eq_ignore_ascii_case(icon_name))?;
        read_model(icon.data()).map(SaveIcon::Model)
    }
}
