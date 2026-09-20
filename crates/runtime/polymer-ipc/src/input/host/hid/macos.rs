use crate::xpc::{Connection, Listener, Packet};
use std::io;
use tokio::{sync::mpsc, task};

pub(super) struct Server(Listener);
impl Drop for Server {
    fn drop(&mut self) {
        self.0.close();
    }
}
impl Server {
    pub fn bind(name: &str) -> io::Result<Self> {
        Listener::bind(name).map(Self)
    }
    pub async fn connect(self) -> io::Result<(Reader, Writer)> {
        let listener = self.0.clone();
        let connection = task::spawn_blocking(move || listener.accept())
            .await
            .map_err(io::Error::other)??;
        let (sender, incoming) = mpsc::channel(32);
        let receiver = connection.clone();
        task::spawn_blocking(move || {
            loop {
                let packet = receiver.receive(None);
                let failed = packet.is_err();
                if sender.blocking_send(packet).is_err() || failed {
                    break;
                }
            }
        });
        Ok((
            Reader {
                connection: connection.clone(),
                incoming,
            },
            Writer(connection),
        ))
    }
}

pub(super) struct Reader {
    connection: Connection,
    incoming: mpsc::Receiver<io::Result<Packet>>,
}
impl Drop for Reader {
    fn drop(&mut self) {
        self.connection.close();
    }
}
impl Reader {
    pub async fn read(&mut self, output: &mut [u8]) -> io::Result<usize> {
        let Some(packet) = self.incoming.recv().await else {
            return Ok(0);
        };
        let packet = packet?;
        if packet.data.len() > output.len()
            || packet.file.is_some()
            || !packet.surfaces.is_empty()
            || packet.event.is_some()
            || packet.consumer_event.is_some()
        {
            return Err(io::Error::new(
                io::ErrorKind::InvalidData,
                "invalid HID report packet",
            ));
        }
        output[..packet.data.len()].copy_from_slice(&packet.data);
        Ok(packet.data.len())
    }
}
pub(super) struct Writer(Connection);
impl Writer {
    pub async fn write_all(&mut self, bytes: &[u8]) -> io::Result<()> {
        let connection = self.0.clone();
        let packet = Packet {
            data: bytes.to_vec(),
            ..Default::default()
        };
        task::spawn_blocking(move || connection.send(packet, None))
            .await
            .map_err(io::Error::other)?
    }
}
