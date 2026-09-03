use std::{collections::VecDeque, io::Cursor, num::NonZeroU32, time::SystemTime};

#[derive(Clone, Debug, serde::Serialize)]
#[serde(rename_all = "camelCase")]
pub struct SaveStateMetadata {
    pub slot: u32,
    pub timestamp: u64,
    pub record_id: String,
    pub has_screenshot: bool,
}

#[derive(Clone, Debug)]
pub struct RgbaScreenshot {
    pub width: u32,
    pub height: u32,
    pub pixels: Vec<u8>,
}

#[derive(Debug)]
struct SaveStateRecord {
    metadata: SaveStateMetadata,
    screenshot: Option<Vec<u8>>,
}

#[derive(Debug)]
pub struct SaveStateManager {
    max: NonZeroU32,
    next_record: u64,
    records: VecDeque<SaveStateRecord>,
}

impl SaveStateManager {
    pub fn new(max: NonZeroU32) -> Self {
        Self {
            max,
            next_record: 1,
            records: VecDeque::with_capacity(max.get() as usize),
        }
    }

    pub fn next_slot(&self) -> u32 {
        if self.records.len() == self.max.get() as usize {
            return self
                .records
                .front()
                .expect("a full state queue is nonempty")
                .metadata
                .slot;
        }
        (0..self.max.get())
            .find(|slot| {
                self.records
                    .iter()
                    .all(|record| record.metadata.slot != *slot)
            })
            .expect("a non-full state queue has an unused slot")
    }

    pub fn contains(&self, slot: u32) -> bool {
        self.records
            .iter()
            .any(|record| record.metadata.slot == slot)
    }

    pub fn commit(
        &mut self,
        slot: u32,
        completed: SystemTime,
        screenshot: Option<RgbaScreenshot>,
    ) -> SaveStateMetadata {
        debug_assert!(slot < self.max.get());
        self.records.retain(|record| record.metadata.slot != slot);
        let screenshot = screenshot.and_then(|screenshot| match screenshot.encode_png() {
            Ok(png) => Some(png),
            Err(error) => {
                tracing::warn!("snowglass: save-state screenshot encoding failed: {error}");
                None
            }
        });
        let timestamp = completed
            .duration_since(SystemTime::UNIX_EPOCH)
            .unwrap_or_default()
            .as_millis()
            .try_into()
            .unwrap_or(u64::MAX);
        let metadata = SaveStateMetadata {
            slot,
            timestamp,
            record_id: format!("{:016x}", self.next_record),
            has_screenshot: screenshot.is_some(),
        };
        self.next_record = self.next_record.wrapping_add(1).max(1);
        self.records.push_back(SaveStateRecord {
            metadata: metadata.clone(),
            screenshot,
        });
        while self.records.len() > self.max.get() as usize {
            self.records.pop_front();
        }
        metadata
    }

    pub fn metadata(&self) -> Vec<SaveStateMetadata> {
        self.records
            .iter()
            .rev()
            .map(|record| record.metadata.clone())
            .collect()
    }

    pub fn metadata_for_slot(&self, slot: u32) -> Option<SaveStateMetadata> {
        self.records
            .iter()
            .find(|record| record.metadata.slot == slot)
            .map(|record| record.metadata.clone())
    }

    pub fn screenshot(&self, record_id: &str) -> Option<&[u8]> {
        self.records
            .iter()
            .find(|record| record.metadata.record_id == record_id)
            .and_then(|record| record.screenshot.as_deref())
    }
}

impl RgbaScreenshot {
    pub fn encode_png(self) -> Result<Vec<u8>, image::ImageError> {
        if self.width == 0
            || self.height == 0
            || self.pixels.len() != self.width as usize * self.height as usize * 4
        {
            return Err(image::ImageError::Parameter(
                image::error::ParameterError::from_kind(
                    image::error::ParameterErrorKind::DimensionMismatch,
                ),
            ));
        }
        let mut bytes = Cursor::new(Vec::new());
        image::write_buffer_with_format(
            &mut bytes,
            &self.pixels,
            self.width,
            self.height,
            image::ExtendedColorType::Rgba8,
            image::ImageFormat::Png,
        )?;
        Ok(bytes.into_inner())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn fifo_reuses_the_oldest_slot_without_reaging_resumed_states() {
        let mut states = SaveStateManager::new(NonZeroU32::new(3).unwrap());
        for expected in [0, 1, 2, 0] {
            let slot = states.next_slot();
            assert_eq!(slot, expected);
            states.commit(slot, SystemTime::UNIX_EPOCH, None);
        }
        assert_eq!(
            states
                .metadata()
                .iter()
                .map(|state| state.slot)
                .collect::<Vec<_>>(),
            vec![0, 2, 1]
        );
        assert_eq!(states.metadata_for_slot(2).unwrap().slot, 2);
        assert_eq!(states.next_slot(), 1);
    }

    #[test]
    fn invalid_screenshot_does_not_prevent_state_commit() {
        let mut states = SaveStateManager::new(NonZeroU32::new(1).unwrap());
        let metadata = states.commit(
            0,
            SystemTime::UNIX_EPOCH,
            Some(RgbaScreenshot {
                width: 10,
                height: 10,
                pixels: Vec::new(),
            }),
        );
        assert!(!metadata.has_screenshot);
        assert!(states.contains(0));
    }
}
