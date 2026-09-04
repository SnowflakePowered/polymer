use crossfire::{
    MRx, MTx, TryRecvError, mpmc,
    oneshot::{self, TxOneshot},
};
use parking_lot::Mutex;
use polymer_ipc::{LocalSocket, LocalSocketListener};
use std::{
    sync::{
        Arc,
        atomic::{AtomicBool, Ordering},
    },
    thread::{self, JoinHandle},
    time::Duration,
};

type RequestTx = MTx<mpmc::List<UiRequest>>;
pub type UiRequestReceiver = MRx<mpmc::List<UiRequest>>;

pub struct UiRequest {
    pub request: polymer_ipc::ui::MessageBoxRequest,
    response: TxOneshot<polymer_ipc::ui::MessageBoxResponse>,
}

impl UiRequest {
    pub fn respond(
        self,
        response: polymer_ipc::ui::MessageBoxResponse,
    ) -> Result<(), polymer_ipc::ui::MessageBoxResponse> {
        if self.response.is_disconnected() {
            return Err(response);
        }
        self.response.send(response);
        Ok(())
    }
}

pub(crate) struct UiTransport {
    shared: Arc<UiShared>,
    worker: Mutex<Option<JoinHandle<()>>>,
}

struct UiShared {
    listener: LocalSocketListener,
    socket: Mutex<Option<LocalSocket>>,
    stopping: AtomicBool,
}

impl UiTransport {
    pub(crate) fn bind(
        session: &str,
        wake: Arc<dyn Fn() + Send + Sync>,
    ) -> std::io::Result<(Self, UiRequestReceiver)> {
        let listener = LocalSocketListener::bind(&format!("{session}-ui"))?;
        let shared = Arc::new(UiShared {
            listener: listener.clone(),
            socket: Mutex::new(None),
            stopping: AtomicBool::new(false),
        });
        let (requests, receiver) = mpmc::unbounded_blocking();
        let worker = spawn_ui_worker(listener, requests, wake, shared.clone())?;
        Ok((
            Self {
                shared,
                worker: Mutex::new(Some(worker)),
            },
            receiver,
        ))
    }

    pub(crate) fn shutdown(&self) {
        if self.shared.stopping.swap(true, Ordering::AcqRel) {
            return;
        }
        self.shared.listener.close();
        if let Some(socket) = self.shared.socket.lock().as_ref() {
            let _ = socket.shutdown();
        }
        if let Some(worker) = self.worker.lock().take() {
            let _ = worker.join();
        }
    }
}

impl Drop for UiTransport {
    fn drop(&mut self) {
        self.shutdown();
    }
}

fn spawn_ui_worker(
    listener: LocalSocketListener,
    requests: RequestTx,
    wake: Arc<dyn Fn() + Send + Sync>,
    shared: Arc<UiShared>,
) -> std::io::Result<JoinHandle<()>> {
    thread::Builder::new()
        .name("polymer-runtime-ui-accept".to_owned())
        .spawn(move || {
            loop {
                let mut socket = match listener.accept() {
                    Ok(socket) => socket,
                    Err(error) => {
                        if !shared.stopping.load(Ordering::Acquire) {
                            tracing::error!("polymer: UI IPC accept ended: {error}");
                        }
                        return;
                    }
                };
                if shared.stopping.load(Ordering::Acquire) {
                    let _ = socket.shutdown();
                    return;
                }
                *shared.socket.lock() = socket.try_clone().ok();
                let result = handle_request(&mut socket, &requests, &wake, &shared.stopping);
                shared.socket.lock().take();
                if let Err(error) = result
                    && !shared.stopping.load(Ordering::Acquire)
                    && error.kind() != std::io::ErrorKind::UnexpectedEof
                    && error.kind() != std::io::ErrorKind::ConnectionReset
                    && error.kind() != std::io::ErrorKind::BrokenPipe
                {
                    tracing::error!("polymer: UI IPC connection ended: {error}");
                }
            }
        })
}

fn handle_request(
    socket: &mut LocalSocket,
    requests: &RequestTx,
    wake: &Arc<dyn Fn() + Send + Sync>,
    stopping: &AtomicBool,
) -> std::io::Result<()> {
    let request: polymer_ipc::ui::MessageBoxRequest = polymer_ipc::ui::receive(socket)?;
    if request.protocol_version != polymer_ipc::ui::UI_PROTOCOL_VERSION {
        return Err(std::io::Error::new(
            std::io::ErrorKind::InvalidData,
            "unsupported UI protocol version",
        ));
    }
    let (response, mut selected) = oneshot::oneshot();
    requests
        .send(UiRequest { request, response })
        .map_err(|_| std::io::Error::other("viewer closed"))?;
    wake();
    let response = loop {
        match selected.try_recv() {
            Ok(response) => break response,
            Err(TryRecvError::Empty) if !stopping.load(Ordering::Acquire) => {
                thread::sleep(Duration::from_millis(1));
            }
            Err(_) => return Err(std::io::Error::other("viewer closed")),
        }
    };
    polymer_ipc::ui::send(socket, &response)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn shutdown_joins_worker_and_releases_listener() {
        let session = format!("polymer-runtime-ui-test-{}", std::process::id());
        let (transport, _) = UiTransport::bind(&session, Arc::new(|| {})).unwrap();
        transport.shutdown();
        let replacement = LocalSocketListener::bind(&format!("{session}-ui")).unwrap();
        replacement.close();
    }
}
