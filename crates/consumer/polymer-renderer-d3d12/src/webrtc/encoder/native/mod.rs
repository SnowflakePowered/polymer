//! D3D12 Video Process + Video Encode HEVC/H.264 backend.
#![allow(unsafe_code)]

use std::{
    ffi::c_void,
    mem::{ManuallyDrop, size_of},
    os::windows::io::{AsRawHandle, FromRawHandle, OwnedHandle},
    ptr,
};

use windows::{
    Win32::{
        Foundation::{E_POINTER, HANDLE, RECT, WAIT_OBJECT_0},
        Graphics::{Direct3D12::*, Dxgi::Common::*},
        Media::MediaFoundation::*,
        System::Threading::{CreateEventW, WaitForSingleObject},
    },
    core::Interface,
};

use super::super::D3d12Frame;
use polymer_renderer::webrtc::{Codec, NativeEncoder};

mod bitstream;
mod setup;
mod submit;
mod support;
use bitstream::*;
use submit::*;
use support::*;

type Error = Box<dyn std::error::Error + Send + Sync>;

const BITSTREAM_PADDING: u64 = 64 * 1024;

struct CodecSupport {
    metadata_size: u64,
    hevc: Option<HevcSupport>,
}

/// Driver-specific HEVC encoder limits. The codec configuration and the
/// hand-written parameter sets must both be derived from these values: fixed
/// coding-unit or transform-unit sizes are rejected by drivers whose hardware
/// uses different block sizes.
#[derive(Clone, Copy)]
struct HevcSupport {
    configuration: D3D12_VIDEO_ENCODER_CODEC_CONFIGURATION_HEVC,
    subregion_block_size: u32,
}

// windows-rs 0.61 predates this binary-compatible extension from current
// DirectX-Headers. Keeping the definition local lets us use feature query 48
// without forcing the rest of the workspace onto a new windows-rs release.
#[repr(C)]
struct CodecResourceRequirements1 {
    node_index: u32,
    codec: D3D12_VIDEO_ENCODER_CODEC,
    profile: D3D12_VIDEO_ENCODER_PROFILE_DESC,
    input_format: DXGI_FORMAT,
    picture_target_resolution: D3D12_VIDEO_ENCODER_PICTURE_RESOLUTION_DESC,
    is_supported: windows::core::BOOL,
    compressed_bitstream_buffer_access_alignment: u32,
    encoder_metadata_buffer_access_alignment: u32,
    max_encoder_output_metadata_buffer_size: u32,
    optional_metadata: i32,
    codec_configuration: D3D12_VIDEO_ENCODER_CODEC_CONFIGURATION,
    qp_map_texture_dimensions: D3D12_VIDEO_ENCODER_PICTURE_RESOLUTION_DESC,
    satd_map_texture_dimensions: D3D12_VIDEO_ENCODER_PICTURE_RESOLUTION_DESC,
    bit_allocation_map_texture_dimensions: D3D12_VIDEO_ENCODER_PICTURE_RESOLUTION_DESC,
    frame_psnr_components: u32,
    subregions_psnr_components: u32,
    subregions_psnr_resolved_metadata_buffer_size: u32,
}

pub(crate) struct Encoder {
    codec: Codec,
    _device: ID3D12Device,
    _video_device: ID3D12VideoDevice3,
    processor: ID3D12VideoProcessor,
    process_queue: ID3D12CommandQueue,
    process_allocator: ID3D12CommandAllocator,
    process_list: ID3D12VideoProcessCommandList,
    crop_texture: Option<ID3D12Resource>,
    crop_queue: Option<ID3D12CommandQueue>,
    crop_allocator: Option<ID3D12CommandAllocator>,
    crop_list: Option<ID3D12GraphicsCommandList>,
    nv12: ID3D12Resource,
    encoder: ID3D12VideoEncoder,
    encoder_heap: ID3D12VideoEncoderHeap,
    encode_queue: ID3D12CommandQueue,
    encode_allocator: ID3D12CommandAllocator,
    encode_list: ID3D12VideoEncodeCommandList2,
    reconstructed: ID3D12Resource,
    bitstream: ID3D12Resource,
    bitstream_zero: ID3D12Resource,
    bitstream_readback: ID3D12Resource,
    encoded_metadata: ID3D12Resource,
    readback_queue: ID3D12CommandQueue,
    clear_allocator: ID3D12CommandAllocator,
    clear_list: ID3D12GraphicsCommandList,
    readback_allocator: ID3D12CommandAllocator,
    readback_list: ID3D12GraphicsCommandList,
    fence: ID3D12Fence,
    fence_event: OwnedHandle,
    fence_value: u64,
    width: u32,
    height: u32,
    bitrate: u32,
    frame_rate: u32,
    parameter_sets: Vec<u8>,
    bitstream_offset: u64,
}

impl NativeEncoder for Encoder {
    type Frame = D3d12Frame;
    const BACKEND: &'static str = "D3D12 Video Encode";

    fn new_for_frame(
        frame: &D3d12Frame,
        width: u32,
        height: u32,
        bitrate: u32,
        frame_rate: f32,
        preferred: Codec,
    ) -> Result<Self, Error> {
        if preferred == Codec::H264 {
            return Self::new_with_codec(frame, width, height, bitrate, frame_rate, Codec::H264);
        }
        match Self::new_with_codec(frame, width, height, bitrate, frame_rate, Codec::Hevc) {
            Ok(encoder) => Ok(encoder),
            Err(hevc_error) => {
                tracing::warn!("polymer: D3D12 HEVC unavailable; trying H.264: {hevc_error}");
                Self::new_with_codec(frame, width, height, bitrate, frame_rate, Codec::H264)
            }
        }
    }

    fn codec(&self) -> Codec {
        self.codec
    }

    fn encode_frame(
        &mut self,
        frame: &mut D3d12Frame,
        width: u32,
        height: u32,
    ) -> Result<Vec<u8>, Error> {
        if width != self.width || height != self.height {
            return Err("D3D12 encoder resolution changed".into());
        }
        unsafe {
            self.submit_bitstream_clear()
                .map_err(|error| format!("D3D12 bitstream clear failed: {error}"))?;
            self.submit_crop(frame)
                .map_err(|error| format!("D3D12 GPU crop failed: {error}"))?;
            self.submit_conversion(frame)
                .map_err(|error| format!("D3D12 video conversion failed: {error}"))?;
            self.submit_encode()
                .map_err(|error| format!("D3D12 video encode submission failed: {error}"))?;
            self.submit_readback()
                .map_err(|error| format!("D3D12 compressed readback submission failed: {error}"))?;
            self.read_bitstream()
                .map_err(|error| format!("D3D12 compressed readback failed: {error}").into())
        }
    }
}

impl Drop for Encoder {
    fn drop(&mut self) {
        let _ = unsafe {
            wait_fence(
                &self.fence,
                self.fence_value,
                HANDLE(self.fence_event.as_raw_handle()),
            )
        };
    }
}

macro_rules! transition_fn {
    ($name:ident, $list:ty) => {
        unsafe fn $name(
            list: &$list,
            resource: &ID3D12Resource,
            before: D3D12_RESOURCE_STATES,
            after: D3D12_RESOURCE_STATES,
        ) {
            if before == after {
                return;
            }
            let mut barrier = D3D12_RESOURCE_BARRIER {
                Type: D3D12_RESOURCE_BARRIER_TYPE_TRANSITION,
                Flags: D3D12_RESOURCE_BARRIER_FLAG_NONE,
                Anonymous: D3D12_RESOURCE_BARRIER_0 {
                    Transition: ManuallyDrop::new(D3D12_RESOURCE_TRANSITION_BARRIER {
                        pResource: ManuallyDrop::new(Some(resource.clone())),
                        Subresource: D3D12_RESOURCE_BARRIER_ALL_SUBRESOURCES,
                        StateBefore: before,
                        StateAfter: after,
                    }),
                },
            };
            unsafe {
                list.ResourceBarrier(std::slice::from_ref(&barrier));
                let transition = &mut *barrier.Anonymous.Transition;
                ManuallyDrop::drop(&mut transition.pResource);
            }
        }
    };
}

transition_fn!(graphics_transition, ID3D12GraphicsCommandList);
transition_fn!(process_transition, ID3D12VideoProcessCommandList);
transition_fn!(encode_transition, ID3D12VideoEncodeCommandList2);
