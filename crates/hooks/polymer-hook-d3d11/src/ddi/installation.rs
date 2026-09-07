use super::{ffi::*, registry::Registry};
use parking_lot::Mutex;
use polymer_platform_windows::InlineDetour;
use std::sync::OnceLock;
use windows::{
    Wdk::Graphics::Direct3D::*,
    Win32::{Foundation::*, Graphics::Dxgi::*, System::LibraryLoader::*},
    core::{Error, PCWSTR, Result},
};

struct DriverHook {
    address: usize,
    detour: InlineDetour<OpenFn>,
}
static DRIVERS: [OnceLock<DriverHook>; 8] = [const { OnceLock::new() }; 8];
static INSTALLING: Mutex<()> = Mutex::new(());

pub(super) struct Installation;
impl Installation {
    pub fn install() -> Result<()> {
        let _installation = INSTALLING.lock();
        let factory: IDXGIFactory1 = unsafe { CreateDXGIFactory1()? };
        let mut count = 0;
        for index in 0.. {
            let adapter = match unsafe { factory.EnumAdapters1(index) } {
                Ok(adapter) => adapter,
                Err(error) if error.code() == DXGI_ERROR_NOT_FOUND => break,
                Err(error) => return Err(error),
            };
            let desc = unsafe { adapter.GetDesc1()? };
            if desc.Flags & DXGI_ADAPTER_FLAG_SOFTWARE.0 as u32 != 0 {
                continue;
            }
            match Self::adapter(desc.AdapterLuid) {
                Ok(()) => count += 1,
                Err(error) => {
                    tracing::warn!(%error, "D3D11 adapter does not expose the rotation DDI")
                }
            }
        }
        if count == 0 {
            return Err(Error::new(
                DXGI_ERROR_UNSUPPORTED,
                "no D3D11 rotation-capable driver entry point",
            ));
        }
        // Patched adapter/device callbacks may outlive hook teardown.
        let mut module = HMODULE::default();
        unsafe {
            GetModuleHandleExW(
                GET_MODULE_HANDLE_EX_FLAG_FROM_ADDRESS | GET_MODULE_HANDLE_EX_FLAG_PIN,
                PCWSTR(Self::install as *const () as *const u16),
                &mut module,
            )?;
        }
        Ok(())
    }

    fn adapter(luid: LUID) -> Result<()> {
        let mut adapter = D3DKMT_OPENADAPTERFROMLUID {
            AdapterLuid: luid,
            ..Default::default()
        };
        unsafe {
            D3DKMTOpenAdapterFromLuid(&mut adapter).ok()?;
        }
        let mut filename = D3DKMT_UMDFILENAMEINFO {
            Version: KMTUMDVERSION_DX11,
            ..Default::default()
        };
        let mut query = D3DKMT_QUERYADAPTERINFO {
            hAdapter: adapter.hAdapter,
            Type: KMTQAITYPE_UMDRIVERNAME,
            pPrivateDriverData: std::ptr::from_mut(&mut filename).cast(),
            PrivateDriverDataSize: std::mem::size_of_val(&filename) as u32,
        };
        let status = unsafe { D3DKMTQueryAdapterInfo(&mut query) };
        unsafe {
            D3DKMTCloseAdapter(&D3DKMT_CLOSEADAPTER {
                hAdapter: adapter.hAdapter,
            })
            .ok()?;
        }
        status.ok()?;
        // The kernel supplies the adapter's installed driver path. Retain this
        // library reference for the lifetime of its patched entry point.
        let module = unsafe { LoadLibraryW(PCWSTR(filename.UmdFileName.as_ptr()))? };
        let address = unsafe { GetProcAddress(module, windows::core::s!("OpenAdapter10_2")) }
            .ok_or_else(|| Error::new(DXGI_ERROR_UNSUPPORTED, "driver has no OpenAdapter10_2"))?;
        Self::driver(address as *const () as usize)
    }

    fn driver(address: usize) -> Result<()> {
        if let Some(driver) = DRIVERS
            .iter()
            .filter_map(OnceLock::get)
            .find(|driver| driver.address == address)
        {
            if driver.detour.is_enabled() {
                return Ok(());
            }
            return driver
                .detour
                .enable()
                .map_err(|code| Error::from_hresult(windows::core::HRESULT(code)));
        }
        let callbacks: [OpenFn; 8] = [
            Self::open::<0>,
            Self::open::<1>,
            Self::open::<2>,
            Self::open::<3>,
            Self::open::<4>,
            Self::open::<5>,
            Self::open::<6>,
            Self::open::<7>,
        ];
        let index = DRIVERS
            .iter()
            .position(|driver| driver.get().is_none())
            .ok_or_else(|| Error::new(E_FAIL, "D3D11 driver hook capacity exhausted"))?;
        let original: OpenFn = unsafe { std::mem::transmute(address) };
        let _ = DRIVERS[index].set(DriverHook {
            address,
            detour: InlineDetour::new(original, callbacks[index]),
        });
        DRIVERS[index]
            .get()
            .unwrap()
            .detour
            .enable()
            .map_err(|code| Error::from_hresult(windows::core::HRESULT(code)))
    }

    unsafe extern "system" fn open<const INDEX: usize>(args: *mut OpenAdapter) -> i32 {
        let original = DRIVERS[INDEX]
            .get()
            .expect("published driver hook")
            .detour
            .trampoline();
        let result = unsafe { original(args) };
        if result >= 0 {
            unsafe {
                Registry::opened(&mut *args);
            }
        }
        result
    }
}
