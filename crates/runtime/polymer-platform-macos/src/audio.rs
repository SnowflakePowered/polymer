//! macOS audio controls intentionally target the default system output device.

use objc2_core_audio::*;
use polymer_platform::{AudioState, NormalizedVolume};
use std::{io, mem::size_of, ptr::NonNull};

pub struct SystemAudioState;

impl SystemAudioState {
    /// Resolves the current default output on every operation, including after
    /// headphones or a Bluetooth device replace the previous output.
    pub fn read() -> io::Result<AudioState> {
        let device = OutputDevice::default_output()?;
        if device.0 == kAudioObjectUnknown {
            return Ok(AudioState::default());
        }
        let volumes = device.elements(kAudioDevicePropertyVolumeScalar)?;
        let mut volume = 0.0_f32;
        for element in volumes {
            volume = volume.max(device.read::<f32>(kAudioDevicePropertyVolumeScalar, element)?);
        }
        let mut muted = true;
        for element in device.elements(kAudioDevicePropertyMute)? {
            muted &= device.read::<u32>(kAudioDevicePropertyMute, element)? != 0;
        }
        Ok(AudioState {
            available: true,
            volume: NormalizedVolume::new(volume)
                .ok_or_else(|| io::Error::other("invalid system volume"))?,
            muted,
            // This is a device control, not a count of the guest's audio sessions.
            active_sessions: 0,
        })
    }

    pub fn set_volume(volume: NormalizedVolume) -> io::Result<()> {
        OutputDevice::default_output()?.set(kAudioDevicePropertyVolumeScalar, volume.get())
    }

    pub fn set_muted(muted: bool) -> io::Result<()> {
        OutputDevice::default_output()?.set(kAudioDevicePropertyMute, u32::from(muted))
    }
}

struct OutputDevice(AudioObjectID);

impl OutputDevice {
    fn default_output() -> io::Result<Self> {
        let address = AudioObjectPropertyAddress {
            mSelector: kAudioHardwarePropertyDefaultOutputDevice,
            mScope: kAudioObjectPropertyScopeGlobal,
            mElement: kAudioObjectPropertyElementMain,
        };
        Self(kAudioObjectSystemObject as u32)
            .read_address::<u32>(address)
            .map(Self)
    }

    fn address(selector: u32, element: u32) -> AudioObjectPropertyAddress {
        AudioObjectPropertyAddress {
            mSelector: selector,
            mScope: kAudioObjectPropertyScopeOutput,
            mElement: element,
        }
    }

    fn elements(&self, selector: u32) -> io::Result<Vec<u32>> {
        let mut main = Self::address(selector, kAudioObjectPropertyElementMain);
        // SAFETY: the address lives throughout this call.
        if unsafe { AudioObjectHasProperty(self.0, NonNull::from(&mut main)) } {
            return Ok(vec![kAudioObjectPropertyElementMain]);
        }
        let stereo = self.read::<[u32; 2]>(kAudioDevicePropertyPreferredChannelsForStereo, 0)?;
        let mut elements = Vec::new();
        for element in stereo {
            let mut address = Self::address(selector, element);
            if unsafe { AudioObjectHasProperty(self.0, NonNull::from(&mut address)) }
                && !elements.contains(&element)
            {
                elements.push(element);
            }
        }
        if elements.is_empty() {
            return Err(io::Error::new(
                io::ErrorKind::Unsupported,
                "default output has no requested audio control",
            ));
        }
        Ok(elements)
    }

    fn read<T: zerocopy::FromBytes + Default>(&self, selector: u32, element: u32) -> io::Result<T> {
        self.read_address(Self::address(selector, element))
    }

    fn read_address<T: zerocopy::FromBytes + Default>(
        &self,
        mut address: AudioObjectPropertyAddress,
    ) -> io::Result<T> {
        let mut value = T::default();
        let mut size = size_of::<T>() as u32;
        // SAFETY: the initialized output has capacity `size`; FromBytes permits
        // any bytes the driver writes. All callers select a matching scalar/array.
        let status = unsafe {
            AudioObjectGetPropertyData(
                self.0,
                NonNull::from(&mut address),
                0,
                std::ptr::null(),
                NonNull::from(&mut size),
                NonNull::from(&mut value).cast(),
            )
        };
        if status != 0 || size as usize != size_of::<T>() {
            return Err(io::Error::other(format!(
                "reading CoreAudio property {:#x}: status {status}, size {size}",
                address.mSelector
            )));
        }
        Ok(value)
    }

    fn set<T: zerocopy::IntoBytes + Copy>(&self, selector: u32, value: T) -> io::Result<()> {
        let elements = self.elements(selector)?;
        // Check every channel before writing, so read-only outputs fail before
        // one half of a stereo pair has been changed.
        for &element in &elements {
            let mut address = Self::address(selector, element);
            let mut writable = 0;
            let status = unsafe {
                AudioObjectIsPropertySettable(
                    self.0,
                    NonNull::from(&mut address),
                    NonNull::from(&mut writable),
                )
            };
            if status != 0 || writable == 0 {
                return Err(io::Error::new(
                    io::ErrorKind::Unsupported,
                    format!("CoreAudio property {selector:#x} is not writable (status {status})"),
                ));
            }
        }
        for element in elements {
            let mut address = Self::address(selector, element);
            let mut value = value;
            // SAFETY: all callers provide the property's exact scalar type.
            let status = unsafe {
                AudioObjectSetPropertyData(
                    self.0,
                    NonNull::from(&mut address),
                    0,
                    std::ptr::null(),
                    size_of::<T>() as u32,
                    NonNull::from(&mut value).cast(),
                )
            };
            if status != 0 {
                return Err(io::Error::other(format!(
                    "writing CoreAudio property {selector:#x}: status {status}"
                )));
            }
        }
        Ok(())
    }
}
