//! WebSocket proxy and inspector bridge lifetime.

use super::protocol::{InspectorProtocol, ProtocolError, Reply};
use crate::{app::UserEvent, window_manager::EventLoopProxy};
use futures_util::{SinkExt, StreamExt};
use polymer_command::{CommandSink, CommandUnavailable};
use serde_json::{Value, json};
use std::{
    error::Error,
    net::{SocketAddr, TcpListener},
    thread,
};
use tokio::sync::{broadcast, oneshot};
use tokio_tungstenite::{accept_async, connect_async, tungstenite::Message};

pub(crate) struct Bridge {
    address: SocketAddr,
    events: broadcast::Sender<Value>,
    shutdown: Option<oneshot::Sender<()>>,
    thread: Option<thread::JoinHandle<()>>,
}

impl Bridge {
    pub(crate) fn start(
        upstream: String,
        target_url: String,
        proxy: EventLoopProxy<UserEvent>,
    ) -> Result<Self, Box<dyn Error>> {
        let listener = TcpListener::bind(("127.0.0.1", 0))?;
        let address = listener.local_addr()?;
        listener.set_nonblocking(true)?;
        let (shutdown_tx, shutdown_rx) = oneshot::channel();
        let (events, _) = broadcast::channel(32);
        let thread_events = events.clone();
        let thread = thread::Builder::new()
            .name("polymer-elements-inspector".into())
            .spawn(move || {
                let runtime = tokio::runtime::Builder::new_current_thread()
                    .enable_all()
                    .build();
                match runtime {
                    Ok(runtime) => runtime.block_on(Server {
                        listener,
                        upstream,
                        target_url,
                        proxy,
                        events: thread_events,
                        shutdown: shutdown_rx,
                    }
                    .run()),
                    Err(error) => {
                        tracing::error!(%error, "polymer: creating Elements inspector runtime failed")
                    }
                }
            })?;
        Ok(Self {
            address,
            events,
            shutdown: Some(shutdown_tx),
            thread: Some(thread),
        })
    }

    pub(crate) fn websocket_target(&self) -> String {
        format!("{}/devtools/page/polymer", self.address)
    }

    pub(crate) fn document_updated(&self) {
        let _ = self
            .events
            .send(json!({"method": "DOM.documentUpdated", "params": {}}));
    }
}

impl Drop for Bridge {
    fn drop(&mut self) {
        if let Some(shutdown) = self.shutdown.take() {
            let _ = shutdown.send(());
        }
        if let Some(thread) = self.thread.take() {
            let _ = thread.join();
        }
    }
}

struct Server {
    listener: TcpListener,
    upstream: String,
    target_url: String,
    proxy: EventLoopProxy<UserEvent>,
    events: broadcast::Sender<Value>,
    shutdown: oneshot::Receiver<()>,
}

impl Server {
    async fn run(mut self) {
        let listener = match tokio::net::TcpListener::from_std(self.listener) {
            Ok(listener) => listener,
            Err(error) => {
                tracing::error!(%error, "polymer: starting Elements inspector listener failed");
                return;
            }
        };
        loop {
            tokio::select! {
                _ = &mut self.shutdown => break,
                accepted = listener.accept() => match accepted {
                    Ok((stream, _)) => {
                        let proxy = self.proxy.clone();
                        let protocol = InspectorProtocol::new(CommandSink::try_new(move |command| {
                            proxy
                                .send_event(UserEvent::Inspector(command))
                                .map_err(|_| CommandUnavailable)
                        }));
                        let session = Session {
                            upstream: self.upstream.clone(),
                            target_url: self.target_url.clone(),
                            protocol,
                        };
                        let receiver = self.events.subscribe();
                        tokio::spawn(async move {
                            if let Err(error) = session.run(stream, receiver).await {
                                tracing::warn!(%error, "polymer: Elements inspector session ended");
                            }
                        });
                    }
                    Err(error) => tracing::warn!(%error, "polymer: accepting Elements inspector connection failed"),
                }
            }
        }
    }
}

struct Session {
    upstream: String,
    target_url: String,
    protocol: InspectorProtocol,
}

impl Session {
    async fn run(
        self,
        stream: tokio::net::TcpStream,
        mut events: broadcast::Receiver<Value>,
    ) -> Result<(), Box<dyn Error>> {
        let downstream = accept_async(stream).await?;
        let (upstream, _) = connect_async(&self.upstream).await?;
        let (mut downstream_write, mut downstream_read) = downstream.split();
        let (mut upstream_write, mut upstream_read) = upstream.split();
        let mut include_whitespace = false;

        loop {
            tokio::select! {
                message = downstream_read.next() => {
                    let Some(message) = message else { break };
                    let message = message?;
                    if let Message::Text(text) = &message
                        && let Ok(request) = serde_json::from_str::<Value>(text)
                        && Self::handles_locally(&request)
                    {
                        if request.get("method").and_then(Value::as_str) == Some("DOM.enable") {
                            include_whitespace = request
                                .pointer("/params/includeWhitespace")
                                .and_then(Value::as_str)
                                == Some("all");
                        }
                        let responses = self.dispatch(request, include_whitespace).await;
                        for response in responses {
                            downstream_write.send(Message::text(response.to_string())).await?;
                        }
                    } else {
                        upstream_write.send(message).await?;
                    }
                }
                message = upstream_read.next() => {
                    let Some(message) = message else { break };
                    downstream_write.send(message?).await?;
                }
                event = events.recv() => match event {
                    Ok(event) => downstream_write.send(Message::text(event.to_string())).await?,
                    Err(broadcast::error::RecvError::Lagged(_)) => {
                        downstream_write.send(Message::text(
                            json!({"method": "DOM.documentUpdated", "params": {}}).to_string()
                        )).await?;
                    }
                    Err(broadcast::error::RecvError::Closed) => break,
                }
            }
        }
        Ok(())
    }

    fn handles_locally(request: &Value) -> bool {
        request
            .get("method")
            .and_then(Value::as_str)
            .is_some_and(|method| {
                method.starts_with("DOM.")
                    || method.starts_with("CSS.")
                    || method.starts_with("Overlay.")
                    || method.starts_with("Page.")
                    || method == "Schema.getDomains"
            })
    }

    async fn dispatch(&self, request: Value, include_whitespace: bool) -> Vec<Value> {
        let id = request.get("id").cloned().unwrap_or(Value::Null);
        let response = self
            .protocol
            .dispatch(request, self.target_url.clone(), include_whitespace);
        match tokio::time::timeout(std::time::Duration::from_secs(2), response).await {
            Ok(Ok(Reply { result, events })) => {
                let mut messages = vec![match result {
                    Ok(result) => json!({"id": id, "result": result}),
                    Err(error) => Self::protocol_error(id, error),
                }];
                messages.extend(events);
                messages
            }
            Ok(Err(error)) => vec![Self::protocol_error(id, error)],
            Err(_) => vec![Self::protocol_error(
                id,
                ProtocolError::unavailable("Elements inspector timed out"),
            )],
        }
    }

    fn protocol_error(id: Value, error: ProtocolError) -> Value {
        json!({"id": id, "error": {"code": error.code, "message": error.message}})
    }
}
