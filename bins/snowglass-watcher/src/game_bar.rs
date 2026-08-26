use std::io;

use windows_registry::{CURRENT_USER, Key, Value};

use crate::lifecycle::LifecycleAdjustment;

const GAME_BAR_KEY: &str = "Software\\Microsoft\\GameBar";
const GAME_BAR_CONTROLLER_VALUE: &str = "UseNexusForGameBarEnabled";
const PACKAGE_REPOSITORY_KEY: &str = "Software\\Classes\\Local Settings\\Software\\Microsoft\\Windows\\CurrentVersion\\AppModel\\Repository\\Packages";
const GAME_BAR_PACKAGE_PREFIX: &str = "Microsoft.XboxGamingOverlay_";
const MICROSOFT_STORE_PUBLISHER_SUFFIX: &str = "__8wekyb3d8bbwe";
const ERROR_FILE_NOT_FOUND: i32 = 0x8007_0002_u32 as i32;

pub struct GameBarControllerShortcut {
    key: Key,
    previous: Option<Option<Value>>,
}

impl LifecycleAdjustment for GameBarControllerShortcut {
    fn applicable() -> io::Result<bool> {
        let packages = match CURRENT_USER.open(PACKAGE_REPOSITORY_KEY) {
            Ok(packages) => packages,
            Err(error) if error.code().0 == ERROR_FILE_NOT_FOUND => return Ok(false),
            Err(error) => return Err(io::Error::other(error)),
        };

        Ok(packages.keys().map_err(io::Error::other)?.any(|package| {
            package.starts_with(GAME_BAR_PACKAGE_PREFIX)
                && package.ends_with(MICROSOFT_STORE_PUBLISHER_SUFFIX)
        }))
    }

    fn disable() -> io::Result<Self> {
        let key = CURRENT_USER
            .create(GAME_BAR_KEY)
            .map_err(io::Error::other)?;
        let previous = match key.get_value(GAME_BAR_CONTROLLER_VALUE) {
            Ok(value) => Some(value),
            Err(error) if error.code().0 == ERROR_FILE_NOT_FOUND => None,
            Err(error) => return Err(io::Error::other(error)),
        };
        key.set_u32(GAME_BAR_CONTROLLER_VALUE, 0)
            .map_err(io::Error::other)?;
        Ok(Self {
            key,
            previous: Some(previous),
        })
    }

    fn restore(&mut self) -> io::Result<()> {
        let Some(previous) = self.previous.take() else {
            return Ok(());
        };

        match previous {
            Some(value) => self
                .key
                .set_value(GAME_BAR_CONTROLLER_VALUE, &value)
                .map_err(io::Error::other),
            None => match self.key.remove_value(GAME_BAR_CONTROLLER_VALUE) {
                Ok(()) => Ok(()),
                Err(error) if error.code().0 == ERROR_FILE_NOT_FOUND => Ok(()),
                Err(error) => Err(io::Error::other(error)),
            },
        }
    }
}
