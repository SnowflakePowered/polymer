pub(crate) mod elements;
#[cfg(windows)]
mod windows;
#[cfg(windows)]
pub(crate) use windows::InspectorWindow;
#[cfg(target_os = "macos")]
mod macos;
#[cfg(target_os = "macos")]
pub(crate) use macos::InspectorWindow;
use serde::Deserialize;
use std::{
    error::Error,
    io::{Read, Write},
    net::{SocketAddr, TcpStream},
    time::Duration,
};
#[derive(Deserialize)]
#[serde(rename_all = "camelCase")]
struct InspectorTarget {
    web_socket_debugger_url: String,
    url: String,
}

// WebView2 cannot navigate Chromium's internal `devtools://` scheme. Use the
// matching hosted frontend from the WebView2 runtime's Chromium generation.
const DEVTOOLS_FRONTEND_BASE: &str = concat!(
    "https://chrome-devtools-frontend.appspot.com/serve_rev/",
    "@3edf00b60c60c9248990a39a702ca4882809fe06/inspector.html",
);

fn inspector_target(address: SocketAddr) -> Result<InspectorTarget, Box<dyn Error>> {
    let mut stream = TcpStream::connect_timeout(&address, Duration::from_secs(1))?;
    stream.set_read_timeout(Some(Duration::from_secs(1)))?;
    stream.write_all(
        format!("GET /json HTTP/1.0\r\nHost: {address}\r\nConnection: close\r\n\r\n").as_bytes(),
    )?;
    let mut response = Vec::new();
    stream.read_to_end(&mut response)?;
    let body = response
        .windows(4)
        .position(|window| window == b"\r\n\r\n")
        .map(|offset| &response[offset + 4..])
        .ok_or("Deno inspector returned an invalid HTTP response")?;
    let targets: Vec<InspectorTarget> = serde_json::from_slice(body)?;
    targets
        .into_iter()
        .next()
        .ok_or_else(|| "Deno inspector has no active Blitz target".into())
}
