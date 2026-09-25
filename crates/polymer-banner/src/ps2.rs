use std::borrow::Cow;

use zerocopy::{
    FromBytes, Immutable, KnownLayout, Unaligned,
    byteorder::little_endian::{F32, I16, U16, U32},
};

pub(crate) const TEXTURE_SIZE: u32 = 128;
const TEXTURE_PIXELS: usize = TEXTURE_SIZE as usize * TEXTURE_SIZE as usize;
const RAW_TEXTURE_SIZE: usize = TEXTURE_PIXELS * 2;
const FIXED_POINT_SCALE: f32 = 4096.0;
const PS2_COLOR_SCALE: f32 = 128.0;

// On-disk layouts documented by ps2iconsys:
// https://github.com/ticky/ps2iconsys/blob/develop/include/ps2_ps2icon.hpp
#[derive(FromBytes, Immutable, KnownLayout, Unaligned)]
#[repr(C)]
pub struct IconSys {
    magic: [u8; 4],
    reserved_header: [u8; 2],
    title_break: U16,
    reserved_display: U32,
    background_alpha: U32,
    background_colors: [[U32; 4]; 4],
    light_directions: [[F32; 4]; 3],
    light_colors: [[F32; 4]; 3],
    ambient: [F32; 4],
    title: [u8; 68],
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
pub(crate) struct VertexCoord {
    x: I16,
    y: I16,
    z: I16,
    unknown: I16,
}

#[derive(Clone, Copy, FromBytes, Immutable, KnownLayout, Unaligned)]
#[repr(C)]
pub(crate) struct TextureData {
    u: I16,
    v: I16,
    color: [u8; 4],
}

#[derive(FromBytes, Immutable, KnownLayout, Unaligned)]
#[repr(C)]
struct AnimationHeader {
    id_tag: U32,
    frame_length: U32,
    animation_speed: F32,
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
    time: F32,
    value: F32,
}

const _: () = assert!(size_of::<IconSys>() == 964);
const _: () = assert!(size_of::<IconHeader>() == 20);
const _: () = assert!(size_of::<VertexCoord>() == 8);
const _: () = assert!(size_of::<TextureData>() == 8);
const _: () = assert!(size_of::<AnimationHeader>() == 20);
const _: () = assert!(size_of::<FrameData>() == 8);
const _: () = assert!(size_of::<FrameKey>() == 8);

impl IconSys {
    pub const FILE_NAME: &[u8] = b"icon.sys";

    pub fn parse(bytes: &[u8]) -> Option<&Self> {
        Self::ref_from_bytes(bytes)
            .ok()
            .filter(|icon_sys| icon_sys.magic == *b"PS2D")
    }

    /// The two raw Shift-JIS lines of the save title.
    pub fn title_lines(&self) -> [&[u8]; 2] {
        let title = self.title.split(|byte| *byte == 0).next().unwrap_or(&[]);
        let (first, second) = title.split_at(usize::from(self.title_break.get()).min(title.len()));
        [first, second]
    }

    /// The file name of the icon shown in the memory card browser.
    pub fn normal_icon(&self) -> &[u8] {
        self.normal_icon
            .split(|byte| *byte == 0)
            .next()
            .unwrap_or(&[])
    }

    fn lighting(&self) -> Lighting {
        let vector = |value: &[F32; 4]| value.map(F32::get);
        Lighting {
            directions: self.light_directions.each_ref().map(|direction| {
                let [x, y, z, _] = vector(direction);
                let length = (x * x + y * y + z * z).sqrt();
                if length > f32::EPSILON {
                    [x / length, y / length, z / length, 0.0]
                } else {
                    [0.0; 4]
                }
            }),
            colors: self.light_colors.each_ref().map(vector),
            ambient: vector(&self.ambient),
        }
    }
}

impl VertexCoord {
    pub(crate) fn to_f32(self) -> [f32; 3] {
        [
            -f32::from(self.x.get()) / FIXED_POINT_SCALE,
            -f32::from(self.y.get()) / FIXED_POINT_SCALE,
            f32::from(self.z.get()) / FIXED_POINT_SCALE,
        ]
    }
}

impl TextureData {
    pub(crate) fn uv(self) -> [f32; 2] {
        [
            f32::from(self.u.get()) / FIXED_POINT_SCALE,
            f32::from(self.v.get()) / FIXED_POINT_SCALE,
        ]
    }

    pub(crate) fn color(self) -> [f32; 4] {
        self.color
            .map(|channel| (f32::from(channel) / PS2_COLOR_SCALE).min(1.0))
    }
}

/// The browser's directional lights and ambient light, with directions in the
/// renderer's world space.
#[derive(Clone, Copy)]
pub(crate) struct Lighting {
    pub(crate) directions: [[f32; 4]; 3],
    pub(crate) colors: [[f32; 4]; 3],
    pub(crate) ambient: [f32; 4],
}

#[derive(Clone, Copy)]
struct Key {
    time: f32,
    value: f32,
}

/// The weight of one shape over the frames of the animation loop.
#[derive(Clone)]
struct Track {
    shape: usize,
    keys: Vec<Key>,
}

impl Track {
    fn value(&self, frame: f32) -> f32 {
        let next = self.keys.partition_point(|key| key.time <= frame);
        match (
            next.checked_sub(1).map(|index| self.keys[index]),
            self.keys.get(next),
        ) {
            (Some(from), Some(to)) if to.time > from.time => {
                from.value + (to.value - from.value) * (frame - from.time) / (to.time - from.time)
            }
            (Some(from), _) => from.value,
            (None, Some(to)) => to.value,
            (None, None) => 0.0,
        }
    }
}

#[derive(Clone)]
pub(crate) struct Animation {
    frame_length: f32,
    speed: f32,
    tracks: Vec<Track>,
}

impl Animation {
    /// Fills `weights` with each shape's normalized blend weight `seconds`
    /// into the loop, which runs at 60 frames per second scaled by the speed.
    pub(crate) fn weights(&self, seconds: f32, weights: &mut [f32]) {
        let frame = if self.frame_length > 0.0 {
            (seconds * 60.0 * self.speed).rem_euclid(self.frame_length)
        } else {
            0.0
        };
        weights.fill(0.0);
        for track in &self.tracks {
            weights[track.shape] += track.value(frame);
        }
        let sum = weights.iter().sum::<f32>();
        if sum > 0.0 {
            weights.iter_mut().for_each(|weight| *weight /= sum);
        } else {
            weights[0] = 1.0;
        }
    }
}

#[derive(Clone, Copy)]
pub(crate) struct Vertex {
    pub(crate) normal: VertexCoord,
    pub(crate) texture: TextureData,
}

/// A PS2 memory card icon: its `icon.sys` followed by the `.ico` model it names.
pub struct Ps2Icon {
    pub(crate) shapes: usize,
    coords: Vec<VertexCoord>,
    pub(crate) vertices: Vec<Vertex>,
    pub(crate) animation: Animation,
    pub(crate) lighting: Lighting,
    pub(crate) texture: Option<Vec<u8>>,
}

impl Ps2Icon {
    pub const MEDIA_TYPE: &str = "application/x-ps2-icon";
    const FILE_ID: u32 = 0x0001_0000;

    pub fn parse(bytes: &[u8]) -> Option<Self> {
        let (icon_sys, bytes) = bytes.split_at_checked(size_of::<IconSys>())?;
        let lighting = IconSys::parse(icon_sys)?.lighting();
        let mut offset = 0;
        let header = Self::take::<IconHeader>(bytes, &mut offset)?;
        let shapes = usize::try_from(header.animation_shapes.get()).ok()?;
        let vertex_count = usize::try_from(header.vertex_count.get()).ok()?;
        if header.file_id.get() != Self::FILE_ID
            || shapes == 0
            || vertex_count == 0
            || !vertex_count.is_multiple_of(3)
        {
            return None;
        }
        let (coords, vertices) = Self::geometry(bytes, &mut offset, shapes, vertex_count)?;
        let animation = Self::animation(bytes, &mut offset, shapes)?;
        let texture = Self::texture(bytes, &mut offset, header.texture_type.get())?;
        Some(Self {
            shapes,
            coords,
            vertices,
            animation,
            lighting,
            texture,
        })
    }

    pub(crate) fn position(&self, vertex: usize, shape: usize) -> [f32; 3] {
        self.coords[vertex * self.shapes + shape].to_f32()
    }

    fn take<'a, T>(bytes: &'a [u8], offset: &mut usize) -> Option<&'a T>
    where
        T: FromBytes + KnownLayout + Immutable,
    {
        let value = T::ref_from_prefix(bytes.get(*offset..)?).ok()?.0;
        *offset = offset.checked_add(size_of::<T>())?;
        Some(value)
    }

    fn geometry(
        bytes: &[u8],
        offset: &mut usize,
        shapes: usize,
        vertex_count: usize,
    ) -> Option<(Vec<VertexCoord>, Vec<Vertex>)> {
        let coordinate_count = vertex_count.checked_mul(shapes)?;
        let vertex_bytes = coordinate_count
            .checked_mul(size_of::<VertexCoord>())?
            .checked_add(
                vertex_count.checked_mul(size_of::<VertexCoord>() + size_of::<TextureData>())?,
            )?;
        if bytes.len().saturating_sub(*offset) < vertex_bytes + size_of::<AnimationHeader>() {
            return None;
        }
        let mut coords = Vec::with_capacity(coordinate_count);
        let mut vertices = Vec::with_capacity(vertex_count);
        for _ in 0..vertex_count {
            for _ in 0..shapes {
                coords.push(*Self::take::<VertexCoord>(bytes, offset)?);
            }
            vertices.push(Vertex {
                normal: *Self::take::<VertexCoord>(bytes, offset)?,
                texture: *Self::take::<TextureData>(bytes, offset)?,
            });
        }
        Some((coords, vertices))
    }

    fn animation(bytes: &[u8], offset: &mut usize, shapes: usize) -> Option<Animation> {
        let header = Self::take::<AnimationHeader>(bytes, offset)?;
        if header.id_tag.get() != 1 {
            return None;
        }
        let mut tracks = Vec::new();
        for _ in 0..header.frame_count.get() {
            let frame = Self::take::<FrameData>(bytes, offset)?;
            let shape = usize::try_from(frame.shape_id.get()).ok()?;
            if shape >= shapes {
                return None;
            }
            let keys = (0..frame.key_count.get())
                .map(|_| {
                    Self::take::<FrameKey>(bytes, offset).map(|key| Key {
                        time: key.time.get(),
                        value: key.value.get(),
                    })
                })
                .collect::<Option<Vec<_>>>()?;
            tracks.push(Track { shape, keys });
        }
        let speed = header.animation_speed.get();
        Some(Animation {
            frame_length: header.frame_length.get() as f32,
            speed: if speed.is_finite() && speed > 0.0 {
                speed
            } else {
                1.0
            },
            tracks,
        })
    }

    fn texture(bytes: &[u8], offset: &mut usize, texture_type: u32) -> Option<Option<Vec<u8>>> {
        if texture_type & 0b0100 == 0 {
            return Some(None);
        }
        let raw = if texture_type & 0b1000 == 0 {
            Cow::Borrowed(bytes.get(*offset..offset.checked_add(RAW_TEXTURE_SIZE)?)?)
        } else {
            let size = usize::try_from(Self::take::<U32>(bytes, offset)?.get()).ok()?;
            Cow::Owned(Self::decode_rle(
                bytes.get(*offset..offset.checked_add(size)?)?,
            )?)
        };
        let mut rgba = Vec::with_capacity(TEXTURE_PIXELS * 4);
        for pixel in raw.chunks_exact(2) {
            let color = u16::from_le_bytes([pixel[0], pixel[1]]);
            let expand = |channel: u16| ((channel << 3) | (channel >> 2)) as u8;
            rgba.extend_from_slice(&[
                expand(color & 0x1f),
                expand((color >> 5) & 0x1f),
                expand((color >> 10) & 0x1f),
                u8::MAX,
            ]);
        }
        Some(Some(rgba))
    }

    fn decode_rle(bytes: &[u8]) -> Option<Vec<u8>> {
        let mut input = 0;
        let mut output = Vec::with_capacity(RAW_TEXTURE_SIZE);
        while input < bytes.len() {
            let code = u16::from_le_bytes(bytes.get(input..input + 2)?.try_into().ok()?);
            input += 2;
            if code >= 0xff00 {
                let length = (usize::from(!code) + 1).checked_mul(2)?;
                if output.len().checked_add(length)? > RAW_TEXTURE_SIZE {
                    return None;
                }
                output.extend_from_slice(bytes.get(input..input.checked_add(length)?)?);
                input += length;
            } else if code != 0 {
                let pixel: [u8; 2] = bytes.get(input..input + 2)?.try_into().ok()?;
                input += 2;
                if output.len().checked_add(usize::from(code) * 2)? > RAW_TEXTURE_SIZE {
                    return None;
                }
                for _ in 0..code {
                    output.extend_from_slice(&pixel);
                }
            }
        }
        (output.len() == RAW_TEXTURE_SIZE).then_some(output)
    }
}
