use std::io;
use tokio::{
    io::{ReadHalf, WriteHalf},
    net::windows::named_pipe::{NamedPipeServer, PipeMode, ServerOptions},
};

pub(super) struct Server(NamedPipeServer);
impl Server {
    pub fn bind(name: &str) -> io::Result<Self> {
        ServerOptions::new()
            .pipe_mode(PipeMode::Message)
            .create(name)
            .map(Self)
    }
    pub async fn connect(
        self,
    ) -> io::Result<(ReadHalf<NamedPipeServer>, WriteHalf<NamedPipeServer>)> {
        self.0.connect().await?;
        Ok(tokio::io::split(self.0))
    }
}
