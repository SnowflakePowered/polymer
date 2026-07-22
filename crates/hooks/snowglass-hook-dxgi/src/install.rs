//! Early-process detours for the DXGI factory creation exports.

use std::{ffi::c_void, sync::Mutex};

#[link(name = "dxgi")]
unsafe extern "system" {
    #[link_name = "CreateDXGIFactory"]
    pub(crate) fn system_create_factory(
        interface_id: *const c_void,
        factory: *mut *mut c_void,
    ) -> i32;

    #[link_name = "CreateDXGIFactory1"]
    pub(crate) fn system_create_factory1(
        interface_id: *const c_void,
        factory: *mut *mut c_void,
    ) -> i32;

    #[link_name = "CreateDXGIFactory2"]
    pub(crate) fn system_create_factory2(
        flags: u32,
        interface_id: *const c_void,
        factory: *mut *mut c_void,
    ) -> i32;
}

type CreateFactoryInstallation =
    snowglass_hook::TerminalInstallation<dyn crate::hooks::CreateFactoryTerminal>;
type CreateFactory1Installation =
    snowglass_hook::TerminalInstallation<dyn crate::hooks::CreateFactory1Terminal>;
type CreateFactory2Installation =
    snowglass_hook::TerminalInstallation<dyn crate::hooks::CreateFactory2Terminal>;

#[derive(Default)]
struct Installations {
    create_factory: Option<CreateFactoryInstallation>,
    create_factory1: Option<CreateFactory1Installation>,
    create_factory2: Option<CreateFactory2Installation>,
}

static INSTALLATIONS: Mutex<Installations> = Mutex::new(Installations {
    create_factory: None,
    create_factory1: None,
    create_factory2: None,
});

pub(crate) unsafe fn install() -> bool {
    let mut installations = INSTALLATIONS
        .lock()
        .unwrap_or_else(|error| error.into_inner());
    if installations.create_factory.is_some()
        || installations.create_factory1.is_some()
        || installations.create_factory2.is_some()
    {
        if installations.create_factory.is_some()
            && installations.create_factory1.is_some()
            && installations.create_factory2.is_some()
        {
            return true;
        }
        if !uninstall_group(&mut installations) {
            return false;
        }
    }

    let Ok(first) = crate::hooks::install_create_factory_terminal() else {
        return false;
    };
    let second = match crate::hooks::install_create_factory1_terminal() {
        Ok(installed) => installed,
        Err(_) => {
            if first.uninstall().is_err() {
                installations.create_factory = Some(first);
            }
            return false;
        }
    };
    let third = match crate::hooks::install_create_factory2_terminal() {
        Ok(installed) => installed,
        Err(_) => {
            if second.uninstall().is_err() {
                installations.create_factory1 = Some(second);
            }
            if first.uninstall().is_err() {
                installations.create_factory = Some(first);
            }
            return false;
        }
    };

    installations.create_factory = Some(first);
    installations.create_factory1 = Some(second);
    installations.create_factory2 = Some(third);
    true
}

pub(crate) fn uninstall() -> bool {
    let mut installations = INSTALLATIONS
        .lock()
        .unwrap_or_else(|error| error.into_inner());
    uninstall_group(&mut installations)
}

fn uninstall_group(installations: &mut Installations) -> bool {
    let mut success = true;

    if installations
        .create_factory2
        .as_ref()
        .is_some_and(|installed| installed.uninstall().is_ok())
    {
        installations.create_factory2 = None;
    } else if installations.create_factory2.is_some() {
        success = false;
    }
    if installations
        .create_factory1
        .as_ref()
        .is_some_and(|installed| installed.uninstall().is_ok())
    {
        installations.create_factory1 = None;
    } else if installations.create_factory1.is_some() {
        success = false;
    }
    if installations
        .create_factory
        .as_ref()
        .is_some_and(|installed| installed.uninstall().is_ok())
    {
        installations.create_factory = None;
    } else if installations.create_factory.is_some() {
        success = false;
    }

    success
}
