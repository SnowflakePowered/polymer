use polymer_platform::{ProcessExit, ProcessExitReceiver};
use std::os::unix::process::CommandExt;
use std::{
    io,
    process::{Child, Command},
    sync::{Arc, Mutex, mpsc},
    thread::JoinHandle,
};

type ExitResult = Result<ProcessExit, (io::ErrorKind, String)>;
#[derive(Default)]
struct State {
    exit: Option<ExitResult>,
    subscribers: Vec<mpsc::Sender<io::Result<ProcessExit>>>,
}
pub(super) struct Process {
    child: Child,
    state: Arc<Mutex<State>>,
    waiter: Option<JoinHandle<()>>,
    reaped: bool,
}
impl Process {
    pub fn spawn(command: &mut Command) -> io::Result<Self> {
        let mut child = command.process_group(0).spawn()?;
        let pid = child.id();
        let state = Arc::new(Mutex::new(State::default()));
        let shared = state.clone();
        let waiter = match std::thread::Builder::new()
            .name("polymer-macos-process-wait".into())
            .spawn(move || {
                let mut status: libc::siginfo_t = unsafe { std::mem::zeroed() };
                let result = loop {
                    // Keep the leader unreaped until cleanup. Its PID/group ID cannot
                    // be reused while we still own the process-tree termination right.
                    if unsafe {
                        libc::waitid(libc::P_PID, pid, &mut status, libc::WEXITED | libc::WNOWAIT)
                    } == 0
                    {
                        break Ok(if status.si_code == libc::CLD_EXITED {
                            ProcessExit::Exited {
                                code: unsafe { status.si_status() } as u32,
                            }
                        } else {
                            ProcessExit::Signaled {
                                signal: unsafe { status.si_status() },
                            }
                        });
                    }
                    let error = io::Error::last_os_error();
                    if error.kind() != io::ErrorKind::Interrupted {
                        break Err((error.kind(), error.to_string()));
                    }
                };
                let mut state = shared.lock().unwrap();
                for subscriber in state.subscribers.drain(..) {
                    let _ = subscriber.send(
                        result
                            .clone()
                            .map_err(|(kind, message)| io::Error::new(kind, message)),
                    );
                }
                state.exit = Some(result);
            }) {
            Ok(waiter) => waiter,
            Err(error) => {
                let _ = child.kill();
                let _ = child.wait();
                return Err(error);
            }
        };
        Ok(Self {
            child,
            state,
            waiter: Some(waiter),
            reaped: false,
        })
    }
    pub fn id(&self) -> u32 {
        self.child.id()
    }
    pub fn subscribe(&self) -> ProcessExitReceiver {
        let (sender, receiver) = mpsc::channel();
        let mut state = self.state.lock().unwrap();
        if let Some(exit) = &state.exit {
            let _ = sender.send(
                exit.clone()
                    .map_err(|(kind, message)| io::Error::new(kind, message)),
            );
        } else {
            state.subscribers.push(sender);
        }
        receiver
    }
    pub fn terminate(&mut self) -> io::Result<()> {
        if self.reaped {
            return Ok(());
        }
        // The owned leader remains a zombie until this call, preserving group identity.
        if unsafe { libc::kill(-(self.id() as i32), libc::SIGKILL) } != 0 {
            let error = io::Error::last_os_error();
            if error.raw_os_error() != Some(libc::ESRCH) {
                return Err(error);
            }
        }
        if let Some(waiter) = self.waiter.take() {
            waiter
                .join()
                .map_err(|_| io::Error::other("process waiter panicked"))?;
        }
        self.wait_for_group()?;
        self.child.wait()?;
        self.reaped = true;
        Ok(())
    }
    fn wait_for_group(&self) -> io::Result<()> {
        let deadline = std::time::Instant::now() + std::time::Duration::from_secs(10);
        loop {
            let count = unsafe { proc_listpgrppids(self.id() as i32, std::ptr::null_mut(), 0) };
            if count < 0 {
                return Err(io::Error::last_os_error());
            }
            let mut pids = vec![0i32; count as usize + 32];
            let count = unsafe {
                proc_listpgrppids(
                    self.id() as i32,
                    pids.as_mut_ptr().cast(),
                    std::mem::size_of_val(pids.as_slice()) as i32,
                )
            };
            if count < 0 {
                return Err(io::Error::last_os_error());
            }
            pids.truncate(count as usize);
            if pids.iter().all(|pid| *pid == 0 || *pid == self.id() as i32) {
                return Ok(());
            }
            if std::time::Instant::now() >= deadline {
                return Err(io::Error::new(
                    io::ErrorKind::TimedOut,
                    "guest process group has not exited",
                ));
            }
            std::thread::sleep(std::time::Duration::from_millis(10));
        }
    }
}
impl Drop for Process {
    fn drop(&mut self) {
        let _ = self.terminate();
    }
}

unsafe extern "C" {
    fn proc_listpgrppids(group: i32, buffer: *mut std::ffi::c_void, bytes: i32) -> i32;
}
