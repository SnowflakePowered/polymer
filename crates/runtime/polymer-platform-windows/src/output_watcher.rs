use std::io;
use windows::{
    Win32::{
        Foundation::PROPERTYKEY,
        Media::Audio::{
            DEVICE_STATE, EDataFlow, ERole, IMMDeviceEnumerator, IMMNotificationClient,
            IMMNotificationClient_Impl, MMDeviceEnumerator, eConsole, eRender,
        },
        System::Com::{CLSCTX_ALL, COINIT_MULTITHREADED, CoCreateInstance, CoInitializeEx},
    },
    core::{PCWSTR, implement},
};

type ChangeCallback = Box<dyn Fn() + Send + Sync>;

#[implement(IMMNotificationClient)]
struct DefaultOutputNotifier {
    on_change: ChangeCallback,
}

impl IMMNotificationClient_Impl for DefaultOutputNotifier_Impl {
    fn OnDeviceStateChanged(&self, _: &PCWSTR, _: DEVICE_STATE) -> windows::core::Result<()> {
        Ok(())
    }

    fn OnDeviceAdded(&self, _: &PCWSTR) -> windows::core::Result<()> {
        Ok(())
    }

    fn OnDeviceRemoved(&self, _: &PCWSTR) -> windows::core::Result<()> {
        Ok(())
    }

    fn OnDefaultDeviceChanged(
        &self,
        flow: EDataFlow,
        role: ERole,
        _: &PCWSTR,
    ) -> windows::core::Result<()> {
        if flow == eRender && role == eConsole {
            (self.on_change)();
        }
        Ok(())
    }

    fn OnPropertyValueChanged(&self, _: &PCWSTR, _: &PROPERTYKEY) -> windows::core::Result<()> {
        Ok(())
    }
}

/// Reports changes of the default render endpoint until dropped.
pub struct DefaultOutputWatcher {
    devices: IMMDeviceEnumerator,
    client: IMMNotificationClient,
}

impl DefaultOutputWatcher {
    pub fn new(on_change: impl Fn() + Send + Sync + 'static) -> io::Result<Self> {
        // SAFETY: initializes COM for this caller thread. A thread that was already
        // initialized in a different apartment may return RPC_E_CHANGED_MODE, but
        // its COM apartment is still usable for these agile Core Audio objects.
        let _ = unsafe { CoInitializeEx(None, COINIT_MULTITHREADED) };
        // SAFETY: MMDeviceEnumerator is an in-process COM class with no aggregation.
        let devices: IMMDeviceEnumerator =
            unsafe { CoCreateInstance(&MMDeviceEnumerator, None, CLSCTX_ALL)? };
        let client: IMMNotificationClient = DefaultOutputNotifier {
            on_change: Box::new(on_change),
        }
        .into();
        // SAFETY: the client stays registered only while this watcher owns it.
        unsafe { devices.RegisterEndpointNotificationCallback(&client)? };
        Ok(Self { devices, client })
    }
}

impl Drop for DefaultOutputWatcher {
    fn drop(&mut self) {
        // SAFETY: unregisters the client registered in `new`.
        let _ = unsafe { self.devices.UnregisterEndpointNotificationCallback(&self.client) };
    }
}
