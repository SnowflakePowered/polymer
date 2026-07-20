use super::pointer::{DisplayedSurface, PointerRoutingState, PointerTarget};
use snowglass_input::mouse::layout::Extent;
use snowglass_ipc::{LocalSocket, LocalSocketListener, Message};
use std::{
    collections::HashMap,
    sync::{Arc, Mutex, mpsc},
    thread,
};

pub(crate) use snowglass_input_dinput::{DirectInputForwarder, RawKeyboardForwarder};

pub(crate) fn spawn_input_ipc(listener: LocalSocketListener) -> mpsc::Sender<Message> {
    let (sender, input) = mpsc::channel();
    let clients = Arc::new(Mutex::new(Vec::<LocalSocket>::new()));
    let accepted_clients = clients.clone();
    thread::spawn(move || {
        loop {
            let socket = match listener.accept() {
                Ok(socket) => {
                    eprintln!("snowglass: target input IPC connected");
                    socket
                }
                Err(error) => {
                    eprintln!("snowglass: input IPC accept ended: {error}");
                    return;
                }
            };
            accepted_clients
                .lock()
                .unwrap_or_else(|error| error.into_inner())
                .push(socket);
        }
    });
    thread::spawn(move || {
        while let Ok(event) = input.recv() {
            clients
                .lock()
                .unwrap_or_else(|error| error.into_inner())
                .retain_mut(|socket| match socket.send(event) {
                    Ok(()) => true,
                    Err(error) => {
                        if error.kind() != std::io::ErrorKind::BrokenPipe
                            && error.kind() != std::io::ErrorKind::ConnectionReset
                        {
                            eprintln!("snowglass: input IPC send failed: {error}");
                        }
                        false
                    }
                });
        }
    });
    sender
}

#[allow(clippy::too_many_arguments)]
pub(crate) fn cancel_pointer_connection(
    connection: u64,
    routing: &mut PointerRoutingState,
    viewer_extent: Extent,
    surfaces: &[DisplayedSurface],
    targets: &HashMap<u64, PointerTarget>,
    sender: &mpsc::Sender<Message>,
    direct_input: &mut DirectInputForwarder,
) {
    let Some(buttons) = routing.cancel(connection, viewer_extent, surfaces, targets, sender) else {
        return;
    };
    direct_input.release_mouse_buttons(buttons);
}
