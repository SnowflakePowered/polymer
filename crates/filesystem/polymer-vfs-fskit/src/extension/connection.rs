use crate::wire::{Frame, Request, Response};
use objc2::{AnyThread, rc::Retained};
use objc2_foundation::{NSError, NSPOSIXErrorDomain, NSString, NSUUID};
use objc2_fs_kit::{FSGenericURLResource, FSResource, FSVolumeIdentifier};
use polymer_ipc::xpc::Connection as XpcConnection;
use std::{io, time::Duration};

pub(super) struct Resource {
    pub name: String,
    pub token: [u8; 32],
}
impl Resource {
    pub fn parse(resource: &FSResource) -> io::Result<Self> {
        let resource = resource
            .downcast_ref::<FSGenericURLResource>()
            .ok_or_else(|| io::Error::from_raw_os_error(libc::EINVAL))?;
        let text = unsafe { resource.url().absoluteString() }
            .ok_or_else(|| io::Error::from_raw_os_error(libc::EINVAL))?
            .to_string();
        let mut parts = text
            .strip_prefix("polymer-vfs://xpc/")
            .ok_or_else(|| io::Error::from_raw_os_error(libc::EINVAL))?
            .split('/');
        let name = parts.next().unwrap_or("").to_owned();
        let encoded = parts.next().unwrap_or("");
        if parts.next().is_some()
            || encoded.len() != 64
            || !name.starts_with("Polymer-")
            || !name.bytes().all(|c| c.is_ascii_alphanumeric() || c == b'-')
        {
            return Err(io::Error::from_raw_os_error(libc::EINVAL));
        }
        let mut token = [0; 32];
        for (byte, chunk) in token.iter_mut().zip(encoded.as_bytes().chunks_exact(2)) {
            let hex = std::str::from_utf8(chunk)
                .map_err(|_| io::Error::from_raw_os_error(libc::EINVAL))?;
            *byte = u8::from_str_radix(hex, 16)
                .map_err(|_| io::Error::from_raw_os_error(libc::EINVAL))?;
        }
        Ok(Self { name, token })
    }
    pub fn identifier(&self) -> Retained<FSVolumeIdentifier> {
        // Stable for probe and load, without a persisted global identity.
        unsafe {
            // Avoid the byte initializer's array-pointer encoding mismatch
            // with the runtime observed on macOS 27.
            let hex: String = self.token[..16]
                .iter()
                .map(|byte| format!("{byte:02x}"))
                .collect();
            let text = format!(
                "{}-{}-{}-{}-{}",
                &hex[..8],
                &hex[8..12],
                &hex[12..16],
                &hex[16..20],
                &hex[20..]
            );
            let uuid = NSUUID::initWithUUIDString(NSUUID::alloc(), &NSString::from_str(&text))
                .expect("valid UUID derived from resource token");
            FSVolumeIdentifier::initWithUUID(FSVolumeIdentifier::alloc(), &uuid)
        }
    }
}

pub(super) struct Connection(XpcConnection);
impl Connection {
    pub fn connect(resource: &Resource) -> io::Result<Self> {
        let name: String = resource
            .token
            .iter()
            .map(|byte| format!("{byte:02x}"))
            .collect();
        XpcConnection::connect(&name).map(Self)
    }
    pub fn call(&self, request: &Request) -> io::Result<Response> {
        let reply = self
            .0
            .call(&Frame::encode(request)?, Some(Duration::from_secs(30)))?;
        match Frame::decode(&reply)? {
            Response::Error(code) => Err(io::Error::from_raw_os_error(code)),
            response => Ok(response),
        }
    }
}

pub(super) struct Error;
impl Error {
    pub fn native(error: io::Error) -> Retained<NSError> {
        unsafe {
            NSError::errorWithDomain_code_userInfo(
                NSPOSIXErrorDomain,
                error.raw_os_error().unwrap_or(libc::EIO) as isize,
                None,
            )
        }
    }
    pub fn unsupported() -> Retained<NSError> {
        Self::native(io::Error::from_raw_os_error(libc::ENOTSUP))
    }
    pub fn protocol() -> io::Error {
        io::Error::new(io::ErrorKind::InvalidData, "unexpected VFS response")
    }
}
