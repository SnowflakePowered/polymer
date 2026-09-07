use super::{ResourceIdentity, ffi::*};
use dashmap::DashMap;
use parking_lot::RwLock;
use std::{
    cell::RefCell,
    ffi::c_void,
    sync::{
        Arc, LazyLock,
        atomic::{AtomicBool, Ordering},
    },
};

#[derive(Clone, Copy)]
struct Adapter {
    create: DeviceFn,
    close: CloseFn,
}
#[derive(Clone, Copy)]
struct Functions {
    resource: ResourceFn,
    relocate: RelocateFn,
    destroy: DestroyFn,
}
pub(super) struct Device {
    pub handle: usize,
    pub rotate: RotateFn,
    pub alive: AtomicBool,
    functions: RwLock<Functions>,
}
static ADAPTERS: LazyLock<DashMap<usize, Adapter>> = LazyLock::new(DashMap::new);
static DEVICES: LazyLock<DashMap<usize, Arc<Device>>> = LazyLock::new(DashMap::new);
thread_local! { static CAPTURE: RefCell<Option<Vec<ResourceIdentity>>> = const { RefCell::new(None) }; }

pub(super) struct Capture;
impl Capture {
    pub fn begin() -> windows::core::Result<Self> {
        CAPTURE.with(|capture| {
            let mut capture = capture.borrow_mut();
            if capture.is_some() {
                return Err(windows::core::Error::new(
                    windows::Win32::Foundation::E_FAIL,
                    "nested DDI resource capture",
                ));
            }
            *capture = Some(Vec::new());
            Ok(Self)
        })
    }
    pub fn finish(self) -> Vec<ResourceIdentity> {
        CAPTURE.with(|capture| capture.borrow_mut().take().unwrap_or_default())
    }
}
impl Drop for Capture {
    fn drop(&mut self) {
        CAPTURE.with(|capture| {
            capture.borrow_mut().take();
        });
    }
}

pub(super) struct Registry;
impl Registry {
    pub unsafe fn opened(args: &mut OpenAdapter) {
        if args.functions.is_null() {
            return;
        }
        let adapter = Adapter {
            create: unsafe { std::mem::transmute(args.functions.add(1).read()) },
            close: unsafe { std::mem::transmute(args.functions.add(2).read()) },
        };
        ADAPTERS.insert(args.driver, adapter);
        unsafe {
            args.functions
                .add(1)
                .write(Self::create_device as *const () as usize);
            args.functions
                .add(2)
                .write(Self::close_adapter as *const () as usize);
        }
    }
    unsafe extern "system" fn close_adapter(handle: usize) -> i32 {
        let (_, adapter) = ADAPTERS.remove(&handle).expect("registered DDI adapter");
        unsafe { (adapter.close)(handle) }
    }
    unsafe extern "system" fn create_device(adapter: usize, args: *mut CreateDevice) -> i32 {
        let original = ADAPTERS
            .get(&adapter)
            .expect("registered DDI adapter")
            .create;
        let result = unsafe { original(adapter, args) };
        if result < 0 {
            return result;
        }
        let args = unsafe { &mut *args };
        if args.interface >> 16 != 11 || args.functions.is_null() || args.dxgi_functions.is_null() {
            return result;
        }
        if unsafe { args.dxgi_functions.add(ROTATE).read() } == 0 {
            return result;
        }
        let device = Arc::new(Device {
            handle: args.driver,
            rotate: unsafe { std::mem::transmute(args.dxgi_functions.add(ROTATE).read()) },
            alive: AtomicBool::new(true),
            functions: RwLock::new(unsafe { Self::functions(args.functions) }),
        });
        DEVICES.insert(args.driver, device);
        unsafe {
            Self::patch(args.functions);
        }
        result
    }
    unsafe fn functions(table: *mut usize) -> Functions {
        Functions {
            resource: unsafe { std::mem::transmute(table.add(RESOURCE_CREATE).read()) },
            relocate: unsafe { std::mem::transmute(table.add(RELOCATE).read()) },
            destroy: unsafe { std::mem::transmute(table.add(DEVICE_DESTROY).read()) },
        }
    }
    unsafe fn patch(table: *mut usize) {
        unsafe {
            table
                .add(RESOURCE_CREATE)
                .write(Self::create_resource as *const () as usize);
            table
                .add(RELOCATE)
                .write(Self::relocate as *const () as usize);
            table
                .add(DEVICE_DESTROY)
                .write(Self::destroy_device as *const () as usize);
        }
    }
    unsafe extern "system" fn relocate(handle: usize, table: *mut usize) {
        let device = DEVICES.get(&handle).expect("registered DDI device").clone();
        let original = device.functions.read().relocate;
        unsafe {
            original(handle, table);
        }
        // The runtime may relocate an already-patched table or receive fresh
        // callbacks. Do not accidentally save our wrapper as its own original.
        let mut functions = device.functions.write();
        unsafe {
            if table.add(RESOURCE_CREATE).read() != Self::create_resource as *const () as usize {
                functions.resource = std::mem::transmute(table.add(RESOURCE_CREATE).read());
            }
            if table.add(RELOCATE).read() != Self::relocate as *const () as usize {
                functions.relocate = std::mem::transmute(table.add(RELOCATE).read());
            }
            if table.add(DEVICE_DESTROY).read() != Self::destroy_device as *const () as usize {
                functions.destroy = std::mem::transmute(table.add(DEVICE_DESTROY).read());
            }
            Self::patch(table);
        }
    }
    unsafe extern "system" fn destroy_device(handle: usize) {
        let (_, device) = DEVICES.remove(&handle).expect("registered DDI device");
        device.alive.store(false, Ordering::Release);
        let original = device.functions.read().destroy;
        unsafe {
            original(handle);
        }
    }
    unsafe extern "system" fn create_resource(
        handle: usize,
        description: *const c_void,
        resource: usize,
        runtime: usize,
    ) {
        let device = DEVICES.get(&handle).expect("registered DDI device").clone();
        let original = device.functions.read().resource;
        unsafe {
            original(handle, description, resource, runtime);
        }
        CAPTURE.with(|capture| {
            if let Some(capture) = capture.borrow_mut().as_mut() {
                capture.push(ResourceIdentity {
                    device,
                    handle: resource,
                });
            }
        });
    }
}
