use snowglass_ipc::LocalSocketListener;
use std::{sync::mpsc, thread};

pub(crate) struct UiRequest {
    pub request: snowglass_ipc::ui::MessageBoxRequest,
    pub response: mpsc::SyncSender<snowglass_ipc::ui::MessageBoxResponse>,
}

pub(crate) fn spawn_ui(listener: LocalSocketListener, requests: mpsc::Sender<UiRequest>) {
    thread::spawn(move || {
        loop {
            let mut socket = match listener.accept() {
                Ok(socket) => socket,
                Err(error) => {
                    eprintln!("snowglass: UI IPC accept ended: {error}");
                    return;
                }
            };
            let requests = requests.clone();
            thread::spawn(move || {
                let result = (|| -> std::io::Result<()> {
                    let request: snowglass_ipc::ui::MessageBoxRequest =
                        snowglass_ipc::ui::receive(&mut socket)?;
                    if request.protocol_version != snowglass_ipc::ui::UI_PROTOCOL_VERSION {
                        return Err(std::io::Error::new(
                            std::io::ErrorKind::InvalidData,
                            "unsupported UI protocol version",
                        ));
                    }
                    let (response, selected) = mpsc::sync_channel(0);
                    requests
                        .send(UiRequest { request, response })
                        .map_err(|_| std::io::Error::other("viewer closed"))?;
                    let response = selected
                        .recv()
                        .map_err(|_| std::io::Error::other("viewer closed"))?;
                    snowglass_ipc::ui::send(&mut socket, &response)
                })();
                if let Err(error) = result
                    && error.kind() != std::io::ErrorKind::UnexpectedEof
                    && error.kind() != std::io::ErrorKind::ConnectionReset
                    && error.kind() != std::io::ErrorKind::BrokenPipe
                {
                    eprintln!("snowglass: UI IPC connection ended: {error}");
                }
            });
        }
    });
}
