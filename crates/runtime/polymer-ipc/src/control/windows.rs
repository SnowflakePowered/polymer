use super::*;
use std::{
    fs, io, mem,
    net::TcpStream,
    os::windows::io::{AsRawSocket, FromRawSocket},
    path::PathBuf,
    sync::{
        Arc, OnceLock,
        atomic::{AtomicBool, AtomicUsize, Ordering},
    },
    thread,
    time::{Duration, Instant},
};
use windows_sys::Win32::Networking::WinSock::{
    AF_UNIX, INVALID_SOCKET, SOCK_STREAM, SOCKADDR, SOCKADDR_UN, SOCKET, SOMAXCONN, WSADATA,
    WSAGetLastError, WSAStartup, accept, bind, closesocket, connect, listen, socket,
};
const WINSOCK_VERSION_2_2: u16 = 0x0202;
static WINSOCK: OnceLock<i32> = OnceLock::new();
pub(super) type Stream = TcpStream;
#[derive(Clone)]
pub struct LocalSocketListener {
    inner: Arc<ListenerInner>,
}

struct ListenerInner {
    socket: AtomicUsize,
    path: PathBuf,
    path_owned: AtomicBool,
}

// SAFETY: the listener exclusively owns its socket. Moving the socket between
// threads does not duplicate access or invalidate it.
unsafe impl Send for LocalSocketListener {}
// SAFETY: the socket value is atomic. Winsock permits closing a listening
// socket from another thread to interrupt a blocking accept.
unsafe impl Sync for LocalSocketListener {}

impl LocalSocketListener {
    pub const DIRECTORY_ENV: &str = "POLYMER_IPC_DIRECTORY";

    /// The host passes this directory to guests before redirecting their temp
    /// directories, so sockets and shared mappings use host-backed storage.
    pub fn directory() -> PathBuf {
        std::env::var_os(Self::DIRECTORY_ENV)
            .map(PathBuf::from)
            .unwrap_or_else(std::env::temp_dir)
    }

    pub fn bind(name: &str) -> io::Result<Self> {
        initialize_winsock()?;
        let (address, path) = socket_address(name)?;
        let socket = create_socket()?;
        // SAFETY: address points to an initialized SOCKADDR_UN for the duration
        // of the call, and socket is a live AF_UNIX stream socket.
        if unsafe {
            bind(
                socket,
                (&raw const address).cast::<SOCKADDR>(),
                mem::size_of::<SOCKADDR_UN>() as i32,
            )
        } != 0
        {
            let error = winsock_error();
            unsafe { closesocket(socket) };
            return Err(error);
        }
        // SAFETY: socket was successfully bound above.
        if unsafe { listen(socket, SOMAXCONN as i32) } != 0 {
            let error = winsock_error();
            unsafe { closesocket(socket) };
            let _ = fs::remove_file(&path);
            return Err(error);
        }
        Ok(Self {
            inner: Arc::new(ListenerInner {
                socket: AtomicUsize::new(socket),
                path,
                path_owned: AtomicBool::new(true),
            }),
        })
    }

    pub fn accept(&self) -> io::Result<LocalSocket> {
        let socket = self.inner.socket.load(Ordering::Acquire);
        if socket == INVALID_SOCKET {
            return Err(io::Error::new(
                io::ErrorKind::NotConnected,
                "local socket listener is closed",
            ));
        }
        // SAFETY: self.socket is a listening socket. Peer address information is
        // not needed for this local, single-client transport.
        let connected = unsafe { accept(socket, std::ptr::null_mut(), std::ptr::null_mut()) };
        if connected == INVALID_SOCKET {
            return Err(winsock_error());
        }
        // SAFETY: ownership of the accepted Winsock socket moves into TcpStream.
        let stream = unsafe { TcpStream::from_raw_socket(connected as _) };
        Ok(LocalSocket {
            stream,
            incoming: Vec::new(),
        })
    }

    /// Stops this listener and wakes a thread blocked in [`Self::accept`].
    pub fn close(&self) {
        let socket = self.inner.socket.swap(INVALID_SOCKET, Ordering::AcqRel);
        if socket != INVALID_SOCKET {
            // SAFETY: the successful swap assigns this call sole responsibility
            // for closing the live socket.
            unsafe { closesocket(socket) };
        }
        // The acceptor thread owns another Arc while it unwinds. Unlink now so
        // a replacement emulator instance can bind the same session name
        // immediately after close returns.
        if self.inner.path_owned.swap(false, Ordering::AcqRel) {
            let _ = fs::remove_file(&self.inner.path);
        }
    }
}

impl Drop for ListenerInner {
    fn drop(&mut self) {
        let socket = *self.socket.get_mut();
        if socket != INVALID_SOCKET {
            // SAFETY: the final Arc owns the remaining live listening socket.
            unsafe { closesocket(socket) };
            *self.socket.get_mut() = INVALID_SOCKET;
        }
        if *self.path_owned.get_mut() {
            let _ = fs::remove_file(&self.path);
        }
    }
}

impl LocalSocket {
    /// Polls connection termination without consuming control messages or
    /// changing the socket's blocking mode and read/write timeouts.
    pub fn peer_disconnected(&self) -> io::Result<bool> {
        use windows_sys::Win32::Networking::WinSock::{
            POLLERR, POLLHUP, POLLNVAL, POLLRDNORM, WSAPOLLFD, WSAPoll,
        };
        let mut poll = WSAPOLLFD {
            fd: self.stream.as_raw_socket() as SOCKET,
            events: POLLRDNORM,
            revents: 0,
        };
        if unsafe { WSAPoll(&mut poll, 1, 0) } < 0 {
            return Err(winsock_error());
        }
        Ok(poll.revents & (POLLERR | POLLHUP | POLLNVAL) != 0)
    }

    /// A zero timeout attempts once; hook callers schedule their own retries.
    pub fn connect_timeout(name: &str, timeout: Duration) -> io::Result<Self> {
        initialize_winsock()?;
        let (address, _) = socket_address(name)?;
        let deadline = Instant::now() + timeout;
        loop {
            let socket = create_socket()?;
            // SAFETY: address points to an initialized SOCKADDR_UN and socket is
            // a live AF_UNIX stream socket.
            if unsafe {
                connect(
                    socket,
                    (&raw const address).cast::<SOCKADDR>(),
                    mem::size_of::<SOCKADDR_UN>() as i32,
                )
            } == 0
            {
                // SAFETY: ownership of the connected socket moves into TcpStream.
                let stream = unsafe { TcpStream::from_raw_socket(socket as _) };
                return Ok(Self {
                    stream,
                    incoming: Vec::new(),
                });
            }
            let error = winsock_error();
            unsafe { closesocket(socket) };
            if Instant::now() >= deadline {
                return Err(error);
            }
            thread::sleep(CONNECT_RETRY_DELAY);
        }
    }

    /// Buffer partial messages without blocking, returning only complete messages.
    pub fn try_receive(&mut self) -> io::Result<Option<Message>> {
        use windows_sys::Win32::Networking::WinSock::{FIONREAD, ioctlsocket};
        let mut available = 0;
        if unsafe {
            ioctlsocket(
                self.stream.as_raw_socket() as SOCKET,
                FIONREAD,
                &mut available,
            )
        } != 0
        {
            return Err(io::Error::from_raw_os_error(unsafe { WSAGetLastError() }));
        }
        let available = self.incoming.len() + available as usize;
        let header_len = mem::size_of::<WireMessage>();
        self.read_control_until(available.min(header_len))?;
        if self.incoming.len() < header_len {
            return Ok(None);
        }
        let wire = WireMessage::ref_from_bytes(&self.incoming[..header_len])
            .map_err(|_| invalid_data("invalid wire message layout"))?;
        let total = header_len + wire.data_len()?;
        self.read_control_until(available.min(total))?;
        if self.incoming.len() < total {
            return Ok(None);
        }
        self.receive().map(Some)
    }
}
fn initialize_winsock() -> io::Result<()> {
    let result = *WINSOCK.get_or_init(|| {
        let mut data = WSADATA::default();
        // SAFETY: data is valid writable storage for WSADATA.
        unsafe { WSAStartup(WINSOCK_VERSION_2_2, &raw mut data) }
    });
    if result == 0 {
        Ok(())
    } else {
        Err(io::Error::from_raw_os_error(result))
    }
}

fn create_socket() -> io::Result<SOCKET> {
    // SAFETY: the address family, socket type, and default protocol are valid.
    let socket = unsafe { socket(AF_UNIX as i32, SOCK_STREAM, 0) };
    if socket == INVALID_SOCKET {
        Err(winsock_error())
    } else {
        Ok(socket)
    }
}

fn socket_address(name: &str) -> io::Result<(SOCKADDR_UN, PathBuf)> {
    if name.is_empty() || name.contains(['/', '\\']) {
        return Err(io::Error::new(
            io::ErrorKind::InvalidInput,
            "local socket name must be a non-empty file name",
        ));
    }
    let path = LocalSocketListener::directory().join(format!("{name}.sock"));
    let path_bytes = path
        .to_str()
        .ok_or_else(|| io::Error::new(io::ErrorKind::InvalidInput, "non-Unicode socket path"))?
        .as_bytes();
    let mut address = SOCKADDR_UN {
        sun_family: AF_UNIX,
        sun_path: [0; 108],
    };
    if path_bytes.len() >= address.sun_path.len() {
        return Err(io::Error::new(
            io::ErrorKind::InvalidInput,
            "local socket path is too long",
        ));
    }
    for (destination, source) in address.sun_path.iter_mut().zip(path_bytes) {
        *destination = *source as i8;
    }
    Ok((address, path))
}

fn winsock_error() -> io::Error {
    // SAFETY: WSAGetLastError has no preconditions.
    io::Error::from_raw_os_error(unsafe { WSAGetLastError() })
}
