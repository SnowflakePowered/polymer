use openh264::{
    OpenH264API,
    encoder::{BitRate, EncoderConfig, FrameRate, IntraFramePeriod, Profile, UsageType, VuiConfig},
    formats::{RgbaSliceU8, YUVBuffer},
};

pub(super) struct Encoder {
    encoder: openh264::encoder::Encoder,
    parameter_sets: Vec<u8>,
}

impl Encoder {
    pub(super) fn new(bitrate: u32, frame_rate: f32) -> Result<Self, openh264::Error> {
        Ok(Self {
            encoder: openh264::encoder::Encoder::with_api_config(
                OpenH264API::from_source(),
                EncoderConfig::new()
                    .bitrate(BitRate::from_bps(bitrate))
                    .max_frame_rate(FrameRate::from_hz(frame_rate))
                    .usage_type(UsageType::ScreenContentRealTime)
                    .profile(Profile::Baseline)
                    .intra_frame_period(IntraFramePeriod::from_num_frames(120))
                    .vui(VuiConfig::srgb()),
            )?,
            parameter_sets: Vec::new(),
        })
    }

    pub(super) fn encode(
        &mut self,
        rgba: &[u8],
        width: u32,
        height: u32,
    ) -> Result<Vec<u8>, Box<dyn std::error::Error + Send + Sync>> {
        let rgba = RgbaSliceU8::new(rgba, (width as usize, height as usize));
        let yuv = YUVBuffer::from_rgb_source(rgba);
        let access_unit = self.encoder.encode(&yuv)?.to_vec();
        let mut parameter_sets = Vec::new();
        let mut has_sps = false;
        let mut has_pps = false;
        for nal in annex_b_nals(&access_unit) {
            match nal.first().copied().unwrap_or(0) & 0x1f {
                7 => has_sps = true,
                8 => has_pps = true,
                _ => continue,
            }
            parameter_sets.extend_from_slice(&[0, 0, 0, 1]);
            parameter_sets.extend_from_slice(nal);
        }
        if has_sps && has_pps {
            self.parameter_sets = parameter_sets;
            return Ok(access_unit);
        }
        if self.parameter_sets.is_empty() {
            return Ok(access_unit);
        }
        let mut browser_access_unit =
            Vec::with_capacity(self.parameter_sets.len() + access_unit.len());
        browser_access_unit.extend_from_slice(&self.parameter_sets);
        browser_access_unit.extend_from_slice(&access_unit);
        Ok(browser_access_unit)
    }
}

pub(super) fn parameter_sets(
    width: u32,
    height: u32,
    bitrate: u32,
    frame_rate: f32,
) -> Result<Vec<u8>, Box<dyn std::error::Error + Send + Sync>> {
    let mut encoder = Encoder::new(bitrate, frame_rate)?;
    let access_unit = encoder.encode(
        &vec![0; width as usize * height as usize * 4],
        width,
        height,
    )?;
    let mut headers = Vec::new();
    for nal in annex_b_nals(&access_unit) {
        let kind = nal.first().copied().unwrap_or(0) & 0x1f;
        if kind == 7 || kind == 8 {
            headers.extend_from_slice(&[0, 0, 0, 1]);
            headers.extend_from_slice(nal);
        }
    }
    if headers.is_empty() {
        return Err("OpenH264 did not emit SPS/PPS parameter sets".into());
    }
    Ok(headers)
}

fn annex_b_nals(bytes: &[u8]) -> Vec<&[u8]> {
    let mut starts = Vec::new();
    let mut index = 0;
    while index + 3 < bytes.len() {
        let prefix = if bytes[index..].starts_with(&[0, 0, 0, 1]) {
            4
        } else if bytes[index..].starts_with(&[0, 0, 1]) {
            3
        } else {
            index += 1;
            continue;
        };
        starts.push((index, prefix));
        index += prefix;
    }
    starts
        .iter()
        .enumerate()
        .filter_map(|(position, (start, prefix))| {
            let end = starts
                .get(position + 1)
                .map(|(next, _)| *next)
                .unwrap_or(bytes.len());
            (*start + *prefix < end).then_some(&bytes[*start + *prefix..end])
        })
        .collect()
}
