use std::io;

use crate::{game_bar::GameBarControllerShortcut, steam::SteamGuideButtonFocus};

pub trait LifecycleAdjustment {
    fn applicable() -> io::Result<bool>
    where
        Self: Sized;

    fn disable() -> io::Result<Self>
    where
        Self: Sized;

    fn restore(&mut self) -> io::Result<()>;
}

#[derive(Default)]
pub struct LifecycleAdjustments {
    applied: Vec<Box<dyn LifecycleAdjustment>>,
}

impl LifecycleAdjustments {
    pub fn new() -> io::Result<Self> {
        let mut adjustments = Self::default();
        adjustments.disable::<GameBarControllerShortcut>()?;
        adjustments.disable::<SteamGuideButtonFocus>()?;
        Ok(adjustments)
    }

    fn disable<T: LifecycleAdjustment + 'static>(&mut self) -> io::Result<()> {
        if T::applicable()? {
            self.applied.push(Box::new(T::disable()?));
        }
        Ok(())
    }

    fn restore(&mut self) -> io::Result<()> {
        let mut first_error = None;
        while let Some(mut adjustment) = self.applied.pop() {
            if let Err(error) = adjustment.restore()
                && first_error.is_none()
            {
                first_error = Some(error);
            }
        }
        first_error.map_or(Ok(()), Err)
    }
}

impl Drop for LifecycleAdjustments {
    fn drop(&mut self) {
        let _ = self.restore();
    }
}
