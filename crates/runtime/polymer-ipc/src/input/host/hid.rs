use parking_lot::Mutex;
use polymer_input::{ListenerHandle, PlayerIndex, RawHidFilter};
use std::{
    io,
    sync::{
        Arc,
        atomic::{AtomicBool, Ordering},
    },
    time::Duration,
};
use tokio::{
    io::{AsyncReadExt, AsyncWriteExt},
    net::windows::named_pipe::{PipeMode, ServerOptions},
    sync::mpsc,
    task::JoinHandle,
};

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct RawHidSpec {
    pub pipe_name: String,
    pub vendor_id: u16,
    pub product_id: u16,
    pub report_size: usize,
}

#[derive(Clone)]
pub struct RawHidChannel(Arc<Inner>);

struct Inner {
    index: PlayerIndex,
    spec: RawHidSpec,
    reports: mpsc::UnboundedSender<Vec<u8>>,
    capture: Arc<AtomicBool>,
    listener: Arc<Mutex<Option<ListenerHandle>>>,
    worker: Mutex<Option<JoinHandle<()>>>,
}

impl Drop for Inner {
    fn drop(&mut self) {
        if let Some(handle) = self.listener.lock().as_ref() {
            let _ = handle.set_raw_hid_capture(self.index, false);
        }
        if let Some(worker) = self.worker.get_mut().take() {
            worker.abort();
        }
    }
}

impl RawHidChannel {
    pub(crate) fn open(
        index: PlayerIndex,
        spec: RawHidSpec,
        listener: Arc<Mutex<Option<ListenerHandle>>>,
    ) -> io::Result<Self> {
        if spec.report_size == 0 {
            return Err(io::Error::new(
                io::ErrorKind::InvalidInput,
                "raw HID report size is zero",
            ));
        }
        let server = ServerOptions::new()
            .pipe_mode(PipeMode::Message)
            .create(&spec.pipe_name)?;
        let filter = listener
            .lock()
            .as_ref()
            .and_then(|handle| handle.hub().raw_hid_filter(index));
        let (reports, receiver) = mpsc::unbounded_channel();
        let capture = Arc::new(AtomicBool::new(true));
        let inner = Arc::new(Inner {
            index,
            spec: spec.clone(),
            reports,
            capture: capture.clone(),
            listener: listener.clone(),
            worker: Mutex::new(None),
        });
        let worker = tokio::runtime::Handle::try_current()
            .map_err(|_| io::Error::other("raw HID transport requires a Tokio runtime"))?
            .spawn(Self::serve(
                index, spec, server, receiver, capture, filter, listener,
            ));
        *inner.worker.lock() = Some(worker);
        Ok(Self(inner))
    }

    pub fn index(&self) -> PlayerIndex {
        self.0.index
    }

    pub fn spec(&self) -> &RawHidSpec {
        &self.0.spec
    }

    pub fn set_capture(&self, capture: bool) {
        self.0.capture.store(capture, Ordering::Release);
    }

    pub(crate) fn publish(&self, report: &[u8]) {
        let _ = self.0.reports.send(report.to_vec());
    }

    async fn serve(
        index: PlayerIndex,
        spec: RawHidSpec,
        server: tokio::net::windows::named_pipe::NamedPipeServer,
        mut reports: mpsc::UnboundedReceiver<Vec<u8>>,
        capture: Arc<AtomicBool>,
        filter: Option<Arc<dyn RawHidFilter>>,
        listener: Arc<Mutex<Option<ListenerHandle>>>,
    ) {
        if let Err(error) = server.connect().await {
            tracing::warn!(pipe = %spec.pipe_name, %error, "raw HID pipe failed to accept Dolphin");
            return;
        }
        tracing::info!(
            pipe = %spec.pipe_name,
            player = index.get(),
            vendor_id = format_args!("0x{:04x}", spec.vendor_id),
            product_id = format_args!("0x{:04x}", spec.product_id),
            filtered = filter.is_some(),
            "raw HID pipe connected"
        );
        if let Some(handle) = listener.lock().as_ref() {
            let _ = handle.set_raw_hid_capture(index, true);
        }
        let (mut reader, mut writer) = tokio::io::split(server);
        let mut output = vec![0; spec.report_size];
        let mut logged_input = false;
        let mut logged_output = false;
        let mut logged_routing_error = false;
        loop {
            tokio::select! {
                report = reports.recv() => {
                    let Some(report) = report else { break; };
                    let mut frame = vec![0; spec.report_size];
                    let length = report.len().min(frame.len());
                    frame[..length].copy_from_slice(&report[..length]);
                    if let Some(filter) = &filter {
                        filter.filter_input(&mut frame[..length], capture.load(Ordering::Acquire));
                    }
                    if !logged_input {
                        tracing::info!(
                            pipe = %spec.pipe_name,
                            report_id = frame.first().copied().unwrap_or_default(),
                            length,
                            "forwarding first raw HID input report to Dolphin"
                        );
                        logged_input = true;
                    }
                    if writer.write_all(&frame).await.is_err() { break; }
                }
                read = reader.read(&mut output) => {
                    let Ok(length) = read else { break; };
                    if length == 0 { break; }
                    if !logged_output {
                        tracing::info!(
                            pipe = %spec.pipe_name,
                            report_id = output.first().copied().unwrap_or_default(),
                            length,
                            "received first raw HID output report from Dolphin"
                        );
                        logged_output = true;
                    }
                    let report = filter.as_ref().map_or_else(
                        || std::borrow::Cow::Borrowed(&output[..length]),
                        |filter| filter.filter_output(&output[..length], capture.load(Ordering::Acquire)),
                    );
                    if let Some(handle) = listener.lock().as_ref() {
                        if let Err(error) = handle.send_raw_hid(index, &report)
                            && !logged_routing_error
                        {
                            tracing::warn!(
                                pipe = %spec.pipe_name,
                                player = index.get(),
                                %error,
                                "failed to route a raw HID output report to its source device"
                            );
                            logged_routing_error = true;
                        }
                    }
                }
            }
        }
        tracing::info!(pipe = %spec.pipe_name, "raw HID pipe disconnected");
        tokio::time::sleep(Duration::from_secs(5)).await;
        if let Some(handle) = listener.lock().as_ref() {
            let _ = handle.set_raw_hid_capture(index, false);
        }
    }
}
