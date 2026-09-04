use std::collections::{HashMap, HashSet};

use polymer_automation::{AutomationError, WindowId, WindowSnapshot};
use windows::{
    Win32::Foundation::{HWND, LPARAM},
    core::BOOL,
};

use crate::{ProcessId, process_tree_ids};

pub(super) struct WindowRecord {
    pub hwnd: isize,
    pub snapshot: WindowSnapshot,
}

pub(super) struct WindowCatalog {
    root: ProcessId,
    next_id: u64,
    ids: HashMap<isize, WindowId>,
}

impl WindowCatalog {
    pub fn new(root: ProcessId) -> Self {
        Self {
            root,
            next_id: 1,
            ids: HashMap::new(),
        }
    }

    pub fn enumerate(&mut self) -> Result<Vec<WindowRecord>, AutomationError> {
        let processes = process_tree_ids(self.root)
            .map_err(|error| AutomationError::platform(error.to_string()))?;
        if !processes.contains(&self.root) {
            return Err(AutomationError::process_exited());
        }
        let process_ids = processes
            .iter()
            .map(|process| process.0)
            .collect::<HashSet<_>>();
        let threads = self.thread_ids(&process_ids)?;
        if threads.is_empty() {
            return Err(AutomationError::process_exited());
        }
        let mut handles = Vec::new();
        for thread in threads {
            let result = unsafe {
                windows::Win32::UI::WindowsAndMessaging::EnumThreadWindows(
                    thread,
                    Some(Self::collect_window),
                    LPARAM((&raw mut handles) as isize),
                )
            };
            if result == false {
                let error = std::io::Error::last_os_error();
                if error.raw_os_error() != Some(0) {
                    return Err(AutomationError::platform(error.to_string()));
                }
            }
        }
        handles.sort_unstable();
        handles.dedup();
        let mut windows = Vec::new();
        for hwnd in handles {
            if let Some(record) = self.record(hwnd, &process_ids) {
                windows.push(record);
            }
        }
        Ok(windows)
    }

    unsafe extern "system" fn collect_window(hwnd: HWND, parameter: LPARAM) -> BOOL {
        let handles = unsafe { &mut *(parameter.0 as *mut Vec<isize>) };
        handles.push(hwnd.0 as isize);
        true.into()
    }

    fn thread_ids(&self, processes: &HashSet<u32>) -> Result<Vec<u32>, AutomationError> {
        use std::mem;
        use windows::Win32::Foundation::CloseHandle;
        use windows::Win32::System::Diagnostics::ToolHelp::{
            CreateToolhelp32Snapshot, TH32CS_SNAPTHREAD, THREADENTRY32, Thread32First, Thread32Next,
        };

        let snapshot = unsafe { CreateToolhelp32Snapshot(TH32CS_SNAPTHREAD, 0) }
            .map_err(|error| AutomationError::platform(error.to_string()))?;
        let mut entry = THREADENTRY32 {
            dwSize: mem::size_of::<THREADENTRY32>() as u32,
            ..Default::default()
        };
        let mut threads = Vec::new();
        if unsafe { Thread32First(snapshot, &mut entry) }.is_ok() {
            loop {
                if processes.contains(&entry.th32OwnerProcessID) {
                    threads.push(entry.th32ThreadID);
                }
                if unsafe { Thread32Next(snapshot, &mut entry) }.is_err() {
                    break;
                }
            }
        }
        let _ = unsafe { CloseHandle(snapshot) };
        Ok(threads)
    }

    fn record(&mut self, native: isize, processes: &HashSet<u32>) -> Option<WindowRecord> {
        use windows::Win32::UI::WindowsAndMessaging::{
            GetClassNameW, GetWindowTextLengthW, GetWindowTextW, GetWindowThreadProcessId,
            IsWindow, IsWindowVisible,
        };

        let hwnd = HWND(native as *mut _);
        if unsafe { IsWindow(Some(hwnd)) } == false {
            return None;
        }
        let mut process_id = 0;
        unsafe { GetWindowThreadProcessId(hwnd, Some(&mut process_id)) };
        if !processes.contains(&process_id) {
            return None;
        }
        let title_length = unsafe { GetWindowTextLengthW(hwnd) }.max(0) as usize;
        let mut title = vec![0; title_length + 1];
        let copied = unsafe { GetWindowTextW(hwnd, &mut title) }.max(0) as usize;
        title.truncate(copied);
        let mut class_name = vec![0; 256];
        let copied = unsafe { GetClassNameW(hwnd, &mut class_name) }.max(0) as usize;
        class_name.truncate(copied);
        let id = self.ids.entry(native).or_insert_with(|| {
            let id = WindowId(format!("window-{}", self.next_id));
            self.next_id += 1;
            id
        });
        Some(WindowRecord {
            hwnd: native,
            snapshot: WindowSnapshot {
                id: id.clone(),
                process_id,
                title: String::from_utf16_lossy(&title),
                class_name: String::from_utf16_lossy(&class_name),
                visible: unsafe { IsWindowVisible(hwnd) }.as_bool(),
            },
        })
    }
}
