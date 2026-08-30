use parking_lot::Mutex;
use snowglass_input::{ListenerHandle, PlayerIndex, RawHidFilter};
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
    worker: Mutex<Option<JoinHandle<()>>>,
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
        let capture = Arc::new(AtomicBool::new(false));
        let inner = Arc::new(Inner {
            index,
            spec: spec.clone(),
            reports,
            capture: capture.clone(),
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
        if server.connect().await.is_err() {
            return;
        }
        if let Some(handle) = listener.lock().as_ref() {
            let _ = handle.set_raw_hid_capture(index, true);
        }
        let (mut reader, mut writer) = tokio::io::split(server);
        let mut output = vec![0; spec.report_size];
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
                    if writer.write_all(&frame).await.is_err() { break; }
                }
                read = reader.read(&mut output) => {
                    let Ok(length) = read else { break; };
                    if length == 0 { break; }
                    let report = filter.as_ref().map_or_else(
                        || std::borrow::Cow::Borrowed(&output[..length]),
                        |filter| filter.filter_output(&output[..length], capture.load(Ordering::Acquire)),
                    );
                    if let Some(handle) = listener.lock().as_ref() {
                        if handle.send_raw_hid(index, &report).is_err() { break; }
                    }
                }
            }
        }
        tokio::time::sleep(Duration::from_secs(5)).await;
        if let Some(handle) = listener.lock().as_ref() {
            let _ = handle.set_raw_hid_capture(index, false);
        }
    }
}
