use std::{ffi::c_void, mem::size_of, ptr, time::Duration};
use windows::{
    Win32::{
        Foundation::{E_FAIL, E_POINTER, HGLOBAL},
        Graphics::Direct3D12::{D3D12_BOX, ID3D12Device, ID3D12Resource},
        Media::MediaFoundation::*,
        System::Com::{STREAM_SEEK_SET, StructuredStorage::CreateStreamOnHGlobal},
    },
    core::{GUID, Interface},
};

const VIDEO_STREAM: u32 = MF_SOURCE_READER_FIRST_VIDEO_STREAM.0 as u32;
const AUDIO_STREAM: u32 = MF_SOURCE_READER_FIRST_AUDIO_STREAM.0 as u32;

pub(super) struct VideoFormat {
    pub width: u32,
    pub height: u32,
    pub crop: D3D12_BOX,
}

impl VideoFormat {
    unsafe fn read(reader: &IMFSourceReader) -> windows::core::Result<Self> {
        let native = unsafe { reader.GetNativeMediaType(VIDEO_STREAM, 0)? };
        let current = unsafe { reader.GetCurrentMediaType(VIDEO_STREAM)? };
        let packed = unsafe { current.GetUINT64(&MF_MT_FRAME_SIZE)? };
        let (frame_width, frame_height) = ((packed >> 32) as u32, packed as u32);
        let mut area = MFVideoArea::default();
        let blob = unsafe {
            std::slice::from_raw_parts_mut(
                ptr::from_mut(&mut area).cast::<u8>(),
                size_of::<MFVideoArea>(),
            )
        };
        let aperture = [&MF_MT_MINIMUM_DISPLAY_APERTURE, &MF_MT_GEOMETRIC_APERTURE]
            .into_iter()
            .any(|key| unsafe { native.GetBlob(key, blob, None) }.is_ok());
        let (left, top, width, height) = if aperture {
            (
                area.OffsetX.value.max(0) as u32,
                area.OffsetY.value.max(0) as u32,
                area.Area.cx.max(0) as u32,
                area.Area.cy.max(0) as u32,
            )
        } else {
            (0, 0, frame_width, frame_height)
        };
        let right = (left + width).min(frame_width);
        let bottom = (top + height).min(frame_height);
        Ok(Self {
            width: right.saturating_sub(left).max(1),
            height: bottom.saturating_sub(top).max(1),
            crop: D3D12_BOX {
                left,
                top,
                front: 0,
                right,
                bottom,
                back: 1,
            },
        })
    }
}

#[derive(Clone, Copy)]
pub(super) struct AudioFormat {
    pub sample_rate: u32,
    pub channels: u16,
}

impl AudioFormat {
    pub fn duration(self, samples: usize) -> Duration {
        let frames = samples as u64 / u64::from(self.channels.max(1));
        Duration::from_nanos(frames * 1_000_000_000 / u64::from(self.sample_rate.max(1)))
    }
}

pub(super) struct VideoFrame {
    _sample: IMFSample,
    pub resource: ID3D12Resource,
    pub sync: IMFD3D12SynchronizationObjectCommands,
    pub time: Duration,
    pub end: Duration,
}

pub(super) struct MediaReader {
    reader: IMFSourceReader,
    _manager: IMFDXGIDeviceManager,
    pub video: VideoFormat,
    pub audio: Option<AudioFormat>,
    pub duration: Option<Duration>,
    pub reformatted: bool,
}

impl MediaReader {
    pub fn open(device: &ID3D12Device, bytes: &[u8]) -> windows::core::Result<Self> {
        unsafe {
            let mut token = 0;
            let mut manager = None;
            MFCreateDXGIDeviceManager(&mut token, &mut manager)?;
            let manager = manager.ok_or_else(|| windows::core::Error::from_hresult(E_POINTER))?;
            manager.ResetDevice(device, token)?;

            let stream = CreateStreamOnHGlobal(HGLOBAL::default(), true)?;
            stream
                .Write(bytes.as_ptr().cast::<c_void>(), bytes.len() as u32, None)
                .ok()?;
            stream.Seek(0, STREAM_SEEK_SET, None)?;
            let byte_stream = MFCreateMFByteStreamOnStream(&stream)?;

            let mut attributes = None;
            MFCreateAttributes(&mut attributes, 3)?;
            let attributes =
                attributes.ok_or_else(|| windows::core::Error::from_hresult(E_POINTER))?;
            attributes.SetUnknown(&MF_SOURCE_READER_D3D_MANAGER, &manager)?;
            attributes.SetUINT32(&MF_READWRITE_ENABLE_HARDWARE_TRANSFORMS, 1)?;
            attributes.SetUINT32(&MF_SOURCE_READER_ENABLE_ADVANCED_VIDEO_PROCESSING, 1)?;
            let reader = MFCreateSourceReaderFromByteStream(&byte_stream, &attributes)?;

            reader.SetStreamSelection(MF_SOURCE_READER_ALL_STREAMS.0 as u32, false)?;
            reader.SetStreamSelection(VIDEO_STREAM, true)?;
            let output = MFCreateMediaType()?;
            output.SetGUID(&MF_MT_MAJOR_TYPE, &MFMediaType_Video)?;
            output.SetGUID(&MF_MT_SUBTYPE, &MFVideoFormat_ARGB32)?;
            reader.SetCurrentMediaType(VIDEO_STREAM, None, &output)?;
            let video = VideoFormat::read(&reader)?;
            let audio = Self::select_audio(&reader).ok();
            if audio.is_none() {
                let _ = reader.SetStreamSelection(AUDIO_STREAM, false);
            }
            let duration = reader
                .GetPresentationAttribute(MF_SOURCE_READER_MEDIASOURCE.0 as u32, &MF_PD_DURATION)
                .ok()
                .and_then(|value| u64::try_from(&value).ok())
                .map(|value| Self::time(value as i64));
            Ok(Self {
                reader,
                _manager: manager,
                video,
                audio,
                duration,
                reformatted: false,
            })
        }
    }

    unsafe fn select_audio(reader: &IMFSourceReader) -> windows::core::Result<AudioFormat> {
        unsafe {
            reader.SetStreamSelection(AUDIO_STREAM, true)?;
            let output = MFCreateMediaType()?;
            output.SetGUID(&MF_MT_MAJOR_TYPE, &MFMediaType_Audio)?;
            output.SetGUID(&MF_MT_SUBTYPE, &MFAudioFormat_Float)?;
            output.SetUINT32(&MF_MT_AUDIO_NUM_CHANNELS, 2)?;
            reader.SetCurrentMediaType(AUDIO_STREAM, None, &output)?;
            let current = reader.GetCurrentMediaType(AUDIO_STREAM)?;
            Ok(AudioFormat {
                sample_rate: current.GetUINT32(&MF_MT_AUDIO_SAMPLES_PER_SECOND)?,
                channels: current.GetUINT32(&MF_MT_AUDIO_NUM_CHANNELS)? as u16,
            })
        }
    }

    fn time(ticks: i64) -> Duration {
        Duration::from_nanos(ticks.max(0) as u64 * 100)
    }

    unsafe fn read(&self, stream: u32) -> windows::core::Result<Option<(IMFSample, i64, u32)>> {
        let mut seen = 0u32;
        loop {
            let mut flags = 0u32;
            let mut timestamp = 0i64;
            let mut sample = None;
            unsafe {
                self.reader.ReadSample(
                    stream,
                    0,
                    None,
                    Some(&mut flags),
                    Some(&mut timestamp),
                    Some(&mut sample),
                )?;
            }
            seen |= flags;
            if flags & MF_SOURCE_READERF_ERROR.0 as u32 != 0 {
                return Err(windows::core::Error::from_hresult(E_FAIL));
            }
            if flags & MF_SOURCE_READERF_ENDOFSTREAM.0 as u32 != 0 {
                return Ok(None);
            }
            if let Some(sample) = sample {
                return Ok(Some((sample, timestamp, seen)));
            }
        }
    }

    pub fn read_video(&mut self) -> windows::core::Result<Option<VideoFrame>> {
        unsafe {
            let Some((sample, timestamp, flags)) = self.read(VIDEO_STREAM)? else {
                return Ok(None);
            };
            if flags & MF_SOURCE_READERF_CURRENTMEDIATYPECHANGED.0 as u32 != 0 {
                self.video = VideoFormat::read(&self.reader)?;
                self.reformatted = true;
            }
            let buffer: IMFDXGIBuffer = sample.GetBufferByIndex(0)?.cast()?;
            let mut resource: *mut c_void = ptr::null_mut();
            buffer.GetResource(&ID3D12Resource::IID, &mut resource)?;
            let resource = ID3D12Resource::from_raw(resource);
            let mut sync: *mut c_void = ptr::null_mut();
            buffer.GetUnknown(
                &MF_D3D12_SYNCHRONIZATION_OBJECT,
                &IMFD3D12SynchronizationObjectCommands::IID,
                &mut sync,
            )?;
            let sync = IMFD3D12SynchronizationObjectCommands::from_raw(sync);
            let duration = sample.GetSampleDuration().unwrap_or_default();
            Ok(Some(VideoFrame {
                _sample: sample,
                resource,
                sync,
                time: Self::time(timestamp),
                end: Self::time(timestamp + duration),
            }))
        }
    }

    pub fn read_audio(&self) -> windows::core::Result<Option<(Duration, Vec<f32>)>> {
        unsafe {
            let Some((sample, timestamp, _)) = self.read(AUDIO_STREAM)? else {
                return Ok(None);
            };
            let buffer = sample.ConvertToContiguousBuffer()?;
            let mut data = ptr::null_mut();
            let mut length = 0u32;
            buffer.Lock(&mut data, None, Some(&mut length))?;
            let samples =
                std::slice::from_raw_parts(data.cast::<f32>(), length as usize / size_of::<f32>())
                    .to_vec();
            buffer.Unlock()?;
            Ok(Some((Self::time(timestamp), samples)))
        }
    }

    pub fn seek(&self, position: Duration) -> windows::core::Result<()> {
        let ticks = (position.as_nanos() / 100) as i64;
        let position = windows::Win32::System::Com::StructuredStorage::PROPVARIANT::from(ticks);
        unsafe { self.reader.SetCurrentPosition(&GUID::zeroed(), &position) }
    }
}
