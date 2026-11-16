use super::*;

impl UploadArena {
    pub(super) const ALIGNMENT: u64 = 16;
    pub(super) const INITIAL_CAPACITY: u64 = 256 * 1024;

    pub(super) const fn new() -> Self {
        Self {
            buffer: None,
            address: 0,
            mapped: 0,
            capacity: 0,
            used: 0,
        }
    }

    /// Hands the whole arena back at the start of a frame. Safe only once that
    /// frame's fence has been waited on, since the GPU reads these bytes.
    pub(super) const fn reset(&mut self) {
        self.used = 0;
    }

    /// Where `length` bytes starting from `used` land, or `None` when they do
    /// not fit and the arena has to grow first.
    pub(super) const fn placement(used: u64, capacity: u64, length: u64) -> Option<u64> {
        // Rounding up is itself fallible: `used` is bounded by `capacity` in
        // practice, but an unchecked round would panic rather than report the
        // miss it is supposed to report.
        let Some(offset) = used.checked_next_multiple_of(Self::ALIGNMENT) else {
            return None;
        };
        match offset.checked_add(length) {
            Some(end) if end <= capacity => Some(offset),
            _ => None,
        }
    }

    /// The capacity a grown arena needs to hold `length` bytes.
    pub(super) const fn grown_capacity(capacity: u64, length: u64) -> u64 {
        // Geometric, so a steady-state frame stops reallocating after the
        // first few passes rather than growing once per draw call.
        let doubled = capacity.saturating_mul(2);
        let floor = if doubled > Self::INITIAL_CAPACITY {
            doubled
        } else {
            Self::INITIAL_CAPACITY
        };
        let needed = length.next_multiple_of(Self::ALIGNMENT);
        if floor > needed { floor } else { needed }
    }

    /// Copies `bytes` into the arena and returns the GPU address they live at.
    pub(super) fn write(
        &mut self,
        device: &ID3D12Device,
        bytes: &[u8],
        garbage: &mut Vec<ID3D12Resource>,
    ) -> windows::core::Result<u64> {
        let length = (bytes.len() as u64).max(1);
        let mut offset = Self::placement(self.used, self.capacity, length);
        if self.buffer.is_none() || offset.is_none() {
            // The retired buffer stays alive in this frame's garbage, so views
            // already recorded against it remain valid until the frame's fence
            // passes.
            self.replace(device, Self::grown_capacity(self.capacity, length), garbage)?;
            offset = Self::placement(0, self.capacity, length);
        }
        let offset = offset.ok_or_else(|| {
            windows::core::Error::from_hresult(windows::Win32::Foundation::E_OUTOFMEMORY)
        })?;
        // SAFETY: the buffer is mapped for its whole lifetime and the range
        // [offset, offset + bytes.len()) was just checked to fit in it.
        unsafe {
            ptr::copy_nonoverlapping(
                bytes.as_ptr(),
                (self.mapped as *mut u8).add(offset as usize),
                bytes.len(),
            );
        }
        self.used = offset + length;
        Ok(self.address + offset)
    }

    pub(super) fn replace(
        &mut self,
        device: &ID3D12Device,
        capacity: u64,
        garbage: &mut Vec<ID3D12Resource>,
    ) -> windows::core::Result<()> {
        // SAFETY: the resource is created and mapped here and stays owned by
        // this arena until it is retired into the frame's garbage.
        let (buffer, address, mapped) = unsafe {
            let buffer = create_upload_buffer(device, capacity)?;
            let mut mapped = ptr::null_mut();
            // An upload heap may stay mapped for its whole life; the empty read
            // range says the CPU never reads back what it wrote.
            buffer.Map(
                0,
                Some(&D3D12_RANGE { Begin: 0, End: 0 }),
                Some(&mut mapped),
            )?;
            let address = buffer.GetGPUVirtualAddress();
            (buffer, address, mapped)
        };
        self.address = address;
        self.mapped = mapped as usize;
        self.capacity = capacity;
        self.used = 0;
        if let Some(previous) = self.buffer.replace(buffer) {
            garbage.push(previous);
        }
        Ok(())
    }
}

#[allow(clippy::missing_safety_doc)]
pub unsafe fn create_texture(
    device: &ID3D12Device,
    width: u32,
    height: u32,
    format: DXGI_FORMAT,
    state: D3D12_RESOURCE_STATES,
) -> windows::core::Result<ID3D12Resource> {
    let mut resource: Option<ID3D12Resource> = None;
    unsafe {
        device.CreateCommittedResource(
            &D3D12_HEAP_PROPERTIES {
                Type: D3D12_HEAP_TYPE_DEFAULT,
                ..Default::default()
            },
            D3D12_HEAP_FLAG_NONE,
            &D3D12_RESOURCE_DESC {
                Dimension: D3D12_RESOURCE_DIMENSION_TEXTURE2D,
                Width: u64::from(width),
                Height: height,
                DepthOrArraySize: 1,
                MipLevels: 1,
                Format: format,
                SampleDesc: DXGI_SAMPLE_DESC {
                    Count: 1,
                    Quality: 0,
                },
                Layout: D3D12_TEXTURE_LAYOUT_UNKNOWN,
                Flags: D3D12_RESOURCE_FLAG_NONE,
                Alignment: 0,
            },
            state,
            None,
            &mut resource,
        )?;
    }
    resource
        .ok_or_else(|| windows::core::Error::from_hresult(windows::Win32::Foundation::E_POINTER))
}

pub(crate) unsafe fn create_srv(
    device: &ID3D12Device,
    heap: &ID3D12DescriptorHeap,
    stride: u32,
    index: u32,
    resource: &ID3D12Resource,
) {
    let desc = unsafe { resource.GetDesc() };
    unsafe {
        device.CreateShaderResourceView(
            resource,
            Some(&D3D12_SHADER_RESOURCE_VIEW_DESC {
                Format: desc.Format,
                ViewDimension: D3D12_SRV_DIMENSION_TEXTURE2D,
                Shader4ComponentMapping: D3D12_DEFAULT_SHADER_4_COMPONENT_MAPPING,
                Anonymous: D3D12_SHADER_RESOURCE_VIEW_DESC_0 {
                    Texture2D: D3D12_TEX2D_SRV {
                        MipLevels: 1,
                        ..Default::default()
                    },
                },
            }),
            cpu_srv_handle(heap, stride, index),
        )
    };
}

pub(super) unsafe fn create_upload_buffer(
    device: &ID3D12Device,
    size: u64,
) -> windows::core::Result<ID3D12Resource> {
    let mut resource: Option<ID3D12Resource> = None;
    unsafe {
        device.CreateCommittedResource(
            &D3D12_HEAP_PROPERTIES {
                Type: D3D12_HEAP_TYPE_UPLOAD,
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
            D3D12_RESOURCE_STATE_GENERIC_READ,
            None,
            &mut resource,
        )?;
    }
    resource
        .ok_or_else(|| windows::core::Error::from_hresult(windows::Win32::Foundation::E_POINTER))
}

pub(crate) unsafe fn transition(
    list: &ID3D12GraphicsCommandList,
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

pub(super) fn rtv_handle(
    heap: &ID3D12DescriptorHeap,
    stride: u32,
    index: u32,
) -> D3D12_CPU_DESCRIPTOR_HANDLE {
    let mut h = unsafe { heap.GetCPUDescriptorHandleForHeapStart() };
    h.ptr += (stride * index) as usize;
    h
}

pub(super) fn cpu_srv_handle(
    heap: &ID3D12DescriptorHeap,
    stride: u32,
    index: u32,
) -> D3D12_CPU_DESCRIPTOR_HANDLE {
    let mut h = unsafe { heap.GetCPUDescriptorHandleForHeapStart() };
    h.ptr += (stride * index) as usize;
    h
}

pub(super) fn gpu_handle(
    heap: &ID3D12DescriptorHeap,
    stride: u32,
    index: u32,
) -> D3D12_GPU_DESCRIPTOR_HANDLE {
    let mut h = unsafe { heap.GetGPUDescriptorHandleForHeapStart() };
    h.ptr += u64::from(stride * index);
    h
}

pub(super) fn as_bytes<T>(items: &[T]) -> &[u8] {
    unsafe { std::slice::from_raw_parts(items.as_ptr().cast(), size_of_val(items)) }
}
