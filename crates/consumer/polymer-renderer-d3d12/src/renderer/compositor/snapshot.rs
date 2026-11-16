use super::*;

impl D3d12Compositor {
    /// Reads the primary imported surface's last displayed frame as normalized RGBA pixels.
    #[allow(clippy::type_complexity)]
    pub fn snapshot_rgba(&mut self) -> Result<Option<(u32, u32, Vec<u8>)>, Box<dyn Error>> {
        let Some(connection) = self.surfaces.first().map(|surface| surface.connection) else {
            return Ok(None);
        };
        self.snapshot_stream_rgba(connection)
    }

    /// Reads one imported stream's last displayed frame as normalized RGBA pixels.
    #[allow(clippy::type_complexity)]
    pub fn snapshot_stream_rgba(
        &mut self,
        connection: u64,
    ) -> Result<Option<(u32, u32, Vec<u8>)>, Box<dyn Error>> {
        let Some((texture, state, flip_y, quarter_turns)) = self
            .surfaces
            .iter()
            .find(|surface| surface.connection == connection)
            .and_then(|surface| {
                let frame = surface.frames.displayed()?;
                Some((
                    surface
                        .cache
                        .as_ref()
                        .or_else(|| surface.imported.resource(frame.slot))?
                        .clone(),
                    D3D12_RESOURCE_STATE_COMMON,
                    surface.flip_y,
                    surface.quarter_turns,
                ))
            })
        else {
            return Ok(None);
        };
        self.wait_idle()?;
        let device = self.receiver.device();
        let desc = unsafe { texture.GetDesc() };
        let mut footprint = D3D12_PLACED_SUBRESOURCE_FOOTPRINT::default();
        let mut total = 0;
        unsafe {
            device.GetCopyableFootprints(
                &raw const desc,
                0,
                1,
                0,
                Some(&raw mut footprint),
                None,
                None,
                Some(&raw mut total),
            );
        }
        let readback = unsafe { create_readback_buffer(device, total)? };
        let allocator: ID3D12CommandAllocator =
            unsafe { device.CreateCommandAllocator(D3D12_COMMAND_LIST_TYPE_DIRECT)? };
        let list: ID3D12GraphicsCommandList = unsafe {
            device.CreateCommandList(0, D3D12_COMMAND_LIST_TYPE_DIRECT, &allocator, None)?
        };
        unsafe { list.SetName(windows::core::w!("Polymer compositor snapshot"))? };
        unsafe {
            transition(&list, &texture, state, D3D12_RESOURCE_STATE_COPY_SOURCE);
            let mut destination = D3D12_TEXTURE_COPY_LOCATION {
                pResource: ManuallyDrop::new(Some(readback.clone())),
                Type: D3D12_TEXTURE_COPY_TYPE_PLACED_FOOTPRINT,
                Anonymous: D3D12_TEXTURE_COPY_LOCATION_0 {
                    PlacedFootprint: footprint,
                },
            };
            let mut source = D3D12_TEXTURE_COPY_LOCATION {
                pResource: ManuallyDrop::new(Some(texture.clone())),
                Type: D3D12_TEXTURE_COPY_TYPE_SUBRESOURCE_INDEX,
                Anonymous: D3D12_TEXTURE_COPY_LOCATION_0 {
                    SubresourceIndex: 0,
                },
            };
            list.CopyTextureRegion(&destination, 0, 0, 0, &source, None);
            ManuallyDrop::drop(&mut destination.pResource);
            ManuallyDrop::drop(&mut source.pResource);
            transition(&list, &texture, D3D12_RESOURCE_STATE_COPY_SOURCE, state);
            list.Close()?;
            let command: ID3D12CommandList = list.cast()?;
            self.receiver.queue().ExecuteCommandLists(&[Some(command)]);
        }
        self.wait_idle()?;

        let mut mapped = ptr::null_mut();
        unsafe { readback.Map(0, None, Some(&mut mapped))? };
        let bytes = unsafe { std::slice::from_raw_parts(mapped.cast::<u8>(), total as usize) };
        let width = desc.Width as usize;
        let height = desc.Height as usize;
        let row_pitch = footprint.Footprint.RowPitch as usize;
        let mut rgba = Vec::with_capacity(width * height * 4);
        // GetCopyableFootprints need not include padding after the final row.
        for y in 0..height {
            let start = footprint.Offset as usize + y * row_pitch;
            for pixel in bytes[start..start + width * 4].chunks_exact(4) {
                if matches!(
                    desc.Format,
                    DXGI_FORMAT_B8G8R8A8_UNORM | DXGI_FORMAT_B8G8R8A8_UNORM_SRGB
                ) {
                    rgba.extend_from_slice(&[pixel[2], pixel[1], pixel[0], pixel[3]]);
                } else if matches!(
                    desc.Format,
                    DXGI_FORMAT_R8G8B8A8_UNORM | DXGI_FORMAT_R8G8B8A8_UNORM_SRGB
                ) {
                    rgba.extend_from_slice(pixel);
                } else if desc.Format == DXGI_FORMAT_R10G10B10A2_UNORM {
                    let value = u32::from_le_bytes(pixel.try_into().expect("four-byte pixel"));
                    rgba.extend_from_slice(&[
                        ((value & 0x3ff) * 255 / 1023) as u8,
                        (((value >> 10) & 0x3ff) * 255 / 1023) as u8,
                        (((value >> 20) & 0x3ff) * 255 / 1023) as u8,
                        255,
                    ]);
                } else {
                    unsafe { readback.Unmap(0, None) };
                    return Err(format!(
                        "surface screenshot does not support DXGI format {:?}",
                        desc.Format
                    )
                    .into());
                }
            }
        }
        unsafe { readback.Unmap(0, None) };
        let (width, height, rgba) =
            normalize_rgba(width as u32, height as u32, &rgba, flip_y, quarter_turns);
        Ok(Some((width, height, rgba)))
    }
}

pub(super) fn normalize_rgba(
    width: u32,
    height: u32,
    pixels: &[u8],
    flip_y: bool,
    quarter_turns: u8,
) -> (u32, u32, Vec<u8>) {
    let rotation = quarter_turns % 4;
    let (output_width, output_height) = if rotation.is_multiple_of(2) {
        (width, height)
    } else {
        (height, width)
    };
    let mut output = vec![0; pixels.len()];
    for y in 0..output_height {
        for x in 0..output_width {
            let (source_x, mut source_y) = match rotation {
                0 => (x, y),
                1 => (width - 1 - y, x),
                2 => (width - 1 - x, height - 1 - y),
                _ => (y, height - 1 - x),
            };
            if flip_y {
                source_y = height - 1 - source_y;
            }
            let source = ((source_y * width + source_x) * 4) as usize;
            let destination = ((y * output_width + x) * 4) as usize;
            output[destination..destination + 4].copy_from_slice(&pixels[source..source + 4]);
        }
    }
    (output_width, output_height, output)
}

pub(super) unsafe fn create_readback_buffer(
    device: &ID3D12Device,
    size: u64,
) -> windows::core::Result<ID3D12Resource> {
    let mut resource = None;
    unsafe {
        device.CreateCommittedResource(
            &D3D12_HEAP_PROPERTIES {
                Type: D3D12_HEAP_TYPE_READBACK,
                ..Default::default()
            },
            D3D12_HEAP_FLAG_NONE,
            &D3D12_RESOURCE_DESC {
                Dimension: D3D12_RESOURCE_DIMENSION_BUFFER,
                Width: size.max(1),
                Height: 1,
                DepthOrArraySize: 1,
                MipLevels: 1,
                SampleDesc: DXGI_SAMPLE_DESC {
                    Count: 1,
                    Quality: 0,
                },
                Layout: D3D12_TEXTURE_LAYOUT_ROW_MAJOR,
                ..Default::default()
            },
            D3D12_RESOURCE_STATE_COPY_DEST,
            None,
            &mut resource,
        )?;
    }
    resource
        .ok_or_else(|| windows::core::Error::from_hresult(windows::Win32::Foundation::E_POINTER))
}
