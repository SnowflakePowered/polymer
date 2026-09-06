use super::*;
use windows::Win32::Graphics::Direct3D12::*;

impl Tracking {
    /// A newly allocated heap has no observed descriptor contents, even when
    /// the runtime recycles a previous heap's CPU address range.
    pub(super) fn reset_descriptor_range(
        &self,
        base: usize,
        count: u32,
        kind: D3D12_DESCRIPTOR_HEAP_TYPE,
    ) {
        let increment = match kind {
            D3D12_DESCRIPTOR_HEAP_TYPE_CBV_SRV_UAV => self.descriptor_increment.get(),
            D3D12_DESCRIPTOR_HEAP_TYPE_RTV => self.rtv_increment.get(),
            _ => return,
        }
        .copied()
        .unwrap_or(0) as usize;
        let Some(end) = (count as usize)
            .checked_mul(increment)
            .and_then(|size| base.checked_add(size))
        else {
            return;
        };
        self.cpu_descriptors
            .retain(|address, _| *address < base || *address >= end);
        self.rtv_descriptors
            .retain(|address, _| *address < base || *address >= end);
        self.gpu_heaps.retain(|_, heap| {
            let old_end = heap
                .cpu_base
                .saturating_add(heap.count as usize * heap.increment as usize);
            old_end <= base || heap.cpu_base >= end
        });
    }
    /// # Safety
    /// `blob` and `output` are the successful CreateRootSignature arguments.
    pub unsafe fn root_signature_created(
        &self,
        blob: *const c_void,
        length: usize,
        output: *mut *mut c_void,
    ) {
        if output.is_null() || unsafe { *output }.is_null() {
            return;
        }
        // A recycled native identity must not inherit its previous layout if
        // this serialized signature cannot be interpreted by the adapter.
        self.root_signatures.remove(&(unsafe { *output } as usize));
        if blob.is_null() {
            return;
        }
        let mut raw = std::ptr::null_mut();
        if unsafe {
            D3D12CreateVersionedRootSignatureDeserializer(
                blob,
                length,
                &ID3D12VersionedRootSignatureDeserializer::IID,
                &mut raw,
            )
        }
        .is_err()
        {
            return;
        }
        let deserializer = unsafe { ID3D12VersionedRootSignatureDeserializer::from_raw(raw) };
        let Ok(desc) =
            (unsafe { deserializer.GetRootSignatureDescAtVersion(D3D_ROOT_SIGNATURE_VERSION_1_0) })
        else {
            return;
        };
        let Some(desc) = (unsafe { desc.as_ref() }) else {
            return;
        };
        let desc = unsafe { desc.Anonymous.Desc_1_0 };
        let mut tables = Vec::new();
        if desc.NumParameters != 0 && !desc.pParameters.is_null() {
            for parameter in
                unsafe { std::slice::from_raw_parts(desc.pParameters, desc.NumParameters as usize) }
            {
                tables.push(Self::descriptor_ranges(parameter));
            }
        }
        self.root_signatures
            .insert(unsafe { *output } as usize, tables);
    }

    fn descriptor_ranges(parameter: &D3D12_ROOT_PARAMETER) -> Vec<(u32, u32)> {
        if parameter.ParameterType != D3D12_ROOT_PARAMETER_TYPE_DESCRIPTOR_TABLE {
            return Vec::new();
        }
        let table = unsafe { parameter.Anonymous.DescriptorTable };
        if table.pDescriptorRanges.is_null() {
            return Vec::new();
        }
        let mut offset = 0u32;
        let mut ranges = Vec::new();
        for range in unsafe {
            std::slice::from_raw_parts(table.pDescriptorRanges, table.NumDescriptorRanges as usize)
        } {
            let start = if range.OffsetInDescriptorsFromTableStart == u32::MAX {
                offset
            } else {
                range.OffsetInDescriptorsFromTableStart
            };
            // Unbounded/bindless ranges cannot establish which entries a draw
            // accessed. Preserve them as unknown instead of scanning the heap.
            if range.RangeType == D3D12_DESCRIPTOR_RANGE_TYPE_SRV && range.NumDescriptors <= 4096 {
                ranges.push((start, range.NumDescriptors));
            }
            offset = start.saturating_add(range.NumDescriptors);
        }
        ranges
    }

    pub(super) fn table_sources(
        &self,
        signature: usize,
        index: u32,
        handle: u64,
        heaps: &[u64],
    ) -> Vec<TrackedResource> {
        let Some(tables) = self.root_signatures.get(&signature) else {
            return Vec::new();
        };
        let Some(ranges) = tables.get(index as usize) else {
            return Vec::new();
        };
        let increment = u64::from(self.descriptor_increment.get().copied().unwrap_or(0));
        if increment == 0 {
            return Vec::new();
        }
        let Some((remaining, heap_base)) = self.gpu_heaps.iter().find_map(|heap| {
            let info = heap.value();
            let offset = handle.checked_sub(*heap.key())?;
            (offset < u64::from(info.count) * increment && offset.is_multiple_of(increment))
                .then_some((u64::from(info.count) - offset / increment, *heap.key()))
        }) else {
            return Vec::new();
        };
        if !heaps.contains(&heap_base) {
            return Vec::new();
        }
        ranges
            .iter()
            .filter(|&&(offset, count)| u64::from(offset) + u64::from(count) <= remaining)
            .flat_map(|&(offset, count)| {
                (0..count).filter_map(move |entry| {
                    let address = handle.checked_add(
                        u64::from(offset)
                            .checked_add(u64::from(entry))?
                            .checked_mul(increment)?,
                    )?;
                    self.resolve_gpu_descriptor(address)
                })
            })
            .flatten()
            .collect()
    }

    /// # Safety
    /// `heaps` contains the live heap interfaces supplied to SetDescriptorHeaps.
    pub unsafe fn record_descriptor_heaps(
        &self,
        list: *mut c_void,
        count: u32,
        heaps: *const *mut c_void,
    ) {
        let mut bound = Vec::new();
        if !heaps.is_null() {
            for &raw in unsafe { std::slice::from_raw_parts(heaps, count as usize) } {
                if let Some(heap) = unsafe { ID3D12DescriptorHeap::from_raw_borrowed(&raw) } {
                    bound.push(unsafe { heap.GetGPUDescriptorHandleForHeapStart() }.ptr);
                }
            }
        }
        let mut state = self.commands.entry(list as usize).or_default();
        state.bindings.set_heaps(bound);
    }

    /// # Safety
    /// Descriptor range pointers and optional sizes follow CopyDescriptors.
    #[allow(clippy::too_many_arguments)] // Mirrors ID3D12Device::CopyDescriptors.
    pub unsafe fn copy_descriptors(
        &self,
        destination_count: u32,
        destinations: *const D3D12_CPU_DESCRIPTOR_HANDLE,
        destination_sizes: *const u32,
        source_count: u32,
        sources: *const D3D12_CPU_DESCRIPTOR_HANDLE,
        source_sizes: *const u32,
        kind: D3D12_DESCRIPTOR_HEAP_TYPE,
    ) {
        if destinations.is_null()
            || sources.is_null()
            || !matches!(
                kind,
                D3D12_DESCRIPTOR_HEAP_TYPE_CBV_SRV_UAV | D3D12_DESCRIPTOR_HEAP_TYPE_RTV
            )
        {
            return;
        }
        let increment = if kind == D3D12_DESCRIPTOR_HEAP_TYPE_RTV {
            self.rtv_increment.get()
        } else {
            self.descriptor_increment.get()
        }
        .copied()
        .unwrap_or(0) as usize;
        if increment == 0 {
            return;
        }
        let ranges = |count, handles: *const D3D12_CPU_DESCRIPTOR_HANDLE, sizes: *const u32| {
            (0..count as usize).flat_map(move |index| {
                let start = unsafe { (*handles.add(index)).ptr };
                let count = if sizes.is_null() {
                    1
                } else {
                    unsafe { *sizes.add(index) }
                };
                (0..count as usize).map(move |offset| start + offset * increment)
            })
        };
        let sources = ranges(source_count, sources, source_sizes).collect::<Vec<_>>();
        let destinations =
            ranges(destination_count, destinations, destination_sizes).collect::<Vec<_>>();
        if kind == D3D12_DESCRIPTOR_HEAP_TYPE_RTV {
            DescriptorCopies::copy(&self.rtv_descriptors, &sources, &destinations);
        } else {
            DescriptorCopies::copy(&self.cpu_descriptors, &sources, &destinations);
        }
    }
}

impl TrackedResource {
    pub(super) fn at_subresource(&self, subresource: u32) -> Option<Self> {
        let desc = unsafe { self.resource.GetDesc() };
        let levels = u32::from(desc.MipLevels).max(1);
        if subresource >= levels * u32::from(desc.DepthOrArraySize) {
            return None;
        }
        let mip = subresource % levels;
        Some(Self {
            width: (desc.Width >> mip).max(1).try_into().ok()?,
            height: (desc.Height >> mip).max(1),
            subresource,
            mip_layer: (mip, subresource / levels),
            ..self.clone()
        })
    }
    fn layers(&self, mip: u32, first: u32, count: u32) -> Vec<Self> {
        let desc = unsafe { self.resource.GetDesc() };
        let levels = u32::from(desc.MipLevels).max(1);
        if mip >= levels || first >= u32::from(desc.DepthOrArraySize) {
            return Vec::new();
        }
        (first
            ..first
                .saturating_add(count)
                .min(u32::from(desc.DepthOrArraySize)))
            .filter_map(|layer| self.at_subresource(mip + layer * levels))
            .collect()
    }
    pub(super) fn shader_views(&self, desc: Option<&D3D12_SHADER_RESOURCE_VIEW_DESC>) -> Vec<Self> {
        let Some(desc) = desc else {
            return self.layers(0, 0, u32::MAX);
        };
        unsafe {
            match desc.ViewDimension {
                D3D12_SRV_DIMENSION_TEXTURE2D => {
                    self.layers(desc.Anonymous.Texture2D.MostDetailedMip, 0, 1)
                }
                D3D12_SRV_DIMENSION_TEXTURE2DARRAY => {
                    let view = desc.Anonymous.Texture2DArray;
                    self.layers(view.MostDetailedMip, view.FirstArraySlice, view.ArraySize)
                }
                D3D12_SRV_DIMENSION_TEXTURE2DMS => self.layers(0, 0, 1),
                D3D12_SRV_DIMENSION_TEXTURE2DMSARRAY => {
                    let view = desc.Anonymous.Texture2DMSArray;
                    self.layers(0, view.FirstArraySlice, view.ArraySize)
                }
                _ => Vec::new(),
            }
        }
    }
    pub(super) fn render_views(&self, desc: Option<&D3D12_RENDER_TARGET_VIEW_DESC>) -> Vec<Self> {
        let Some(desc) = desc else {
            return self.layers(0, 0, u32::MAX);
        };
        unsafe {
            match desc.ViewDimension {
                D3D12_RTV_DIMENSION_TEXTURE2D => {
                    self.layers(desc.Anonymous.Texture2D.MipSlice, 0, 1)
                }
                D3D12_RTV_DIMENSION_TEXTURE2DARRAY => {
                    let view = desc.Anonymous.Texture2DArray;
                    self.layers(view.MipSlice, view.FirstArraySlice, view.ArraySize)
                }
                D3D12_RTV_DIMENSION_TEXTURE2DMS => self.layers(0, 0, 1),
                D3D12_RTV_DIMENSION_TEXTURE2DMSARRAY => {
                    let view = desc.Anonymous.Texture2DMSArray;
                    self.layers(0, view.FirstArraySlice, view.ArraySize)
                }
                _ => Vec::new(),
            }
        }
    }
}

struct DescriptorCopies;
impl DescriptorCopies {
    fn copy<T: Clone>(descriptors: &DashMap<usize, T>, sources: &[usize], destinations: &[usize]) {
        let values = sources
            .iter()
            .map(|source| descriptors.get(source).map(|entry| entry.clone()))
            .collect::<Vec<_>>();
        for (&destination, source) in destinations.iter().zip(values) {
            if let Some(source) = source {
                descriptors.insert(destination, source);
            } else {
                descriptors.remove(&destination);
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn copied_rtv_ranges_preserve_subresources_and_unresolved_descriptors_clear_bindings() {
        let tracking = Tracking::default();
        tracking.rtv_increment.set(8).unwrap();
        tracking.rtv_descriptors.insert(
            100,
            vec![TrackedTarget {
                key: (1, 3),
                width: 64,
                height: 32,
            }],
        );
        tracking.rtv_descriptors.insert(
            108,
            vec![TrackedTarget {
                key: (2, 4),
                width: 32,
                height: 16,
            }],
        );
        tracking.record_copy_descriptors_simple(
            2,
            D3D12_CPU_DESCRIPTOR_HANDLE { ptr: 200 },
            D3D12_CPU_DESCRIPTOR_HANDLE { ptr: 100 },
            D3D12_DESCRIPTOR_HEAP_TYPE_RTV,
        );
        assert_eq!(tracking.rtv_descriptors.get(&200).unwrap()[0].key, (1, 3));
        assert_eq!(tracking.rtv_descriptors.get(&208).unwrap()[0].key, (2, 4));
        tracking.record_copy_descriptors_simple(
            1,
            D3D12_CPU_DESCRIPTOR_HANDLE { ptr: 200 },
            D3D12_CPU_DESCRIPTOR_HANDLE { ptr: 300 },
            D3D12_DESCRIPTOR_HEAP_TYPE_RTV,
        );
        assert!(!tracking.rtv_descriptors.contains_key(&200));
        assert!(tracking.rtv_descriptors.contains_key(&208));
        tracking.reset_descriptor_range(100, 2, D3D12_DESCRIPTOR_HEAP_TYPE_RTV);
        tracking.record_copy_descriptors_simple(
            1,
            D3D12_CPU_DESCRIPTOR_HANDLE { ptr: 200 },
            D3D12_CPU_DESCRIPTOR_HANDLE { ptr: 108 },
            D3D12_DESCRIPTOR_HEAP_TYPE_RTV,
        );
        assert!(!tracking.rtv_descriptors.contains_key(&200));
        assert!(
            tracking.rtv_descriptors.contains_key(&208),
            "recreated heap cleared an unrelated descriptor"
        );
    }
}
