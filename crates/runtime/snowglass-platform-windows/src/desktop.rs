use windows::{
    Win32::{
        Foundation::GENERIC_ALL,
        Security::SECURITY_ATTRIBUTES,
        System::{
            StationsAndDesktops::{
                CloseDesktop, CreateDesktopW, GetThreadDesktop, HDESK, SetThreadDesktop,
            },
            Threading::GetCurrentThreadId,
        },
    },
    core::PCWSTR,
};

pub struct PrivateDesktop {
    pub(crate) handle: HDESK,
}

pub struct DesktopThreadAttachment {
    original: HDESK,
    restored: bool,
}

// SAFETY: A desktop handle is not thread-affine once no thread is attached to it.
unsafe impl Send for PrivateDesktop {}

impl PrivateDesktop {
    pub fn create(name: &str) -> std::io::Result<Self> {
        if name.encode_utf16().any(|unit| unit == 0) {
            return Err(std::io::Error::new(
                std::io::ErrorKind::InvalidInput,
                "desktop name contains a null character",
            ));
        }
        let name = name.encode_utf16().chain(Some(0)).collect::<Vec<_>>();
        let attributes = SECURITY_ATTRIBUTES {
            nLength: std::mem::size_of::<SECURITY_ATTRIBUTES>() as u32,
            lpSecurityDescriptor: std::ptr::null_mut(),
            bInheritHandle: false.into(),
        };
        let handle = unsafe {
            CreateDesktopW(
                PCWSTR(name.as_ptr()),
                PCWSTR::null(),
                None,
                Default::default(),
                GENERIC_ALL.0,
                Some(&raw const attributes),
            )
        }
        .map_err(|_| std::io::Error::last_os_error())?;
        Ok(Self { handle })
    }

    pub fn create_and_attach_current_thread(
        name: &str,
    ) -> std::io::Result<(Self, DesktopThreadAttachment)> {
        let original = unsafe { GetThreadDesktop(GetCurrentThreadId()) }
            .map_err(|_| std::io::Error::last_os_error())?;
        let desktop = Self::create(name)?;
        if unsafe { SetThreadDesktop(desktop.handle) }.is_err() {
            let error = std::io::Error::last_os_error();
            let _ = unsafe { SetThreadDesktop(original) };
            return Err(error);
        }
        Ok((
            desktop,
            DesktopThreadAttachment {
                original,
                restored: false,
            },
        ))
    }
}

impl Drop for PrivateDesktop {
    fn drop(&mut self) {
        let _ = unsafe { CloseDesktop(self.handle) };
    }
}

impl DesktopThreadAttachment {
    pub fn restore(&mut self) -> std::io::Result<()> {
        if self.restored {
            return Ok(());
        }
        unsafe { SetThreadDesktop(self.original) }.map_err(|_| std::io::Error::last_os_error())?;
        self.restored = true;
        Ok(())
    }
}

impl Drop for DesktopThreadAttachment {
    fn drop(&mut self) {
        if !self.restored {
            let _ = unsafe { SetThreadDesktop(self.original) };
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn private_desktop_attaches_and_restores() -> std::io::Result<()> {
        let original = unsafe { GetThreadDesktop(GetCurrentThreadId()) }
            .map_err(|_| std::io::Error::last_os_error())?;
        let name = format!(
            "snowglass-test-{}-{}",
            std::process::id(),
            std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .unwrap_or_default()
                .as_nanos()
        );
        let (desktop, mut attachment) = PrivateDesktop::create_and_attach_current_thread(&name)?;
        assert_eq!(
            unsafe { GetThreadDesktop(GetCurrentThreadId()) }.unwrap(),
            desktop.handle
        );
        attachment.restore()?;
        assert_eq!(
            unsafe { GetThreadDesktop(GetCurrentThreadId()) }.unwrap(),
            original
        );
        Ok(())
    }
}
