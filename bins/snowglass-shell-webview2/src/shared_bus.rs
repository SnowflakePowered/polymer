//! Windows adapter for the shared Snowglass foreground/background bus.
//!
//! The wire types and validation live in `snowglass-pluginhost`. This module
//! is the deliberately unsafe edge which projects WebView2's mapped allocation
//! as four SPSC lanes. It is kept separate so neither pluginhost nor the DOM
//! implementations need platform handles or raw memory.

use serde_json::Value as JsonValue;
use snowglass_pluginhost::bus::{BUS_MAGIC, BUS_VERSION, BusRecord, FifoError, StructuredValue};
use std::{
    collections::{HashMap, VecDeque},
    ffi::c_void,
    ptr::NonNull,
    sync::Arc,
    sync::atomic::{AtomicU32, AtomicU64, Ordering},
};
use webview2_com::Microsoft::Web::WebView2::Win32::{
    COREWEBVIEW2_SHARED_BUFFER_ACCESS_READ_WRITE, ICoreWebView2_17, ICoreWebView2Environment12,
    ICoreWebView2SharedBuffer,
};
use windows::core::{Interface, PCWSTR};
use wry::{WebView, WebViewExtWindows};

pub const LANE_COUNT: usize = 4;
pub const LANE_HEADER_LEN: usize = 64;
pub const LANE_CAPACITY: usize = 1024 * 1024;
pub const ALLOCATION_LEN: usize = LANE_COUNT * (LANE_HEADER_LEN + LANE_CAPACITY);

pub const FOREGROUND_TO_NATIVE: usize = 0;
pub const NATIVE_TO_FOREGROUND: usize = 1;
pub const BACKGROUND_TO_NATIVE: usize = 2;
pub const NATIVE_TO_BACKGROUND: usize = 3;

pub const RECORD_PLUGIN_INVOKE: u16 = 1;
pub const RECORD_RESOLVE: u16 = 2;
pub const RECORD_REJECT: u16 = 3;
pub const RECORD_CANCEL: u16 = 4;
pub const RECORD_BRIDGE_MESSAGE: u16 = 10;
pub const RECORD_BRIDGE_PROXY: u16 = 11;
pub const RECORD_SHELL_COMMAND: u16 = 12;
pub const TARGET_PLUGIN: u8 = 1;
pub const TARGET_FOREGROUND: u8 = 2;
pub const TARGET_BACKGROUND: u8 = 3;
pub const TARGET_NATIVE: u8 = 4;

const MAGIC_OFFSET: usize = 0;
const VERSION_OFFSET: usize = 4;
const LANE_ID_OFFSET: usize = 6;
const GENERATION_OFFSET: usize = 8;
const CAPACITY_OFFSET: usize = 12;
const READ_OFFSET: usize = 16;
const WRITE_OFFSET: usize = 24;
const FAULT_OFFSET: usize = 32;

/// Owns the COM allocation for longer than all native and script views.
pub struct SharedBus {
    buffer: ICoreWebView2SharedBuffer,
    memory: NonNull<u8>,
    generation: u32,
}

#[derive(Clone)]
pub struct BackgroundBusEndpoint {
    owner: ICoreWebView2SharedBuffer,
    memory: NonNull<u8>,
    wake_native: Arc<dyn Fn() + Send + Sync>,
}

impl BackgroundBusEndpoint {
    pub fn wake_native(&self) {
        (self.wake_native)();
    }

    /// Installs a V8-owned `SharedArrayBuffer` view. The backing-store deleter
    /// retains a COM reference, so WebView2 cannot release the mapping while
    /// the background isolate can still reach it.
    pub fn install(&self, runtime: &mut deno_core::JsRuntime) -> Result<(), String> {
        let owner = Box::new(BackgroundBackingOwner {
            _buffer: self.owner.clone(),
        });
        let owner = Box::into_raw(owner).cast::<c_void>();
        // SAFETY: `memory` addresses the WebView2 allocation retained by the
        // COM reference in `owner`. The callback reclaims exactly that Box when
        // V8 releases its final backing-store reference.
        let backing = unsafe {
            deno_core::v8::SharedArrayBuffer::new_backing_store_from_ptr(
                self.memory.as_ptr().cast(),
                ALLOCATION_LEN,
                release_background_backing,
                owner,
            )
        }
        .make_shared();
        deno_core::scope!(scope, runtime);
        let buffer = deno_core::v8::SharedArrayBuffer::with_backing_store(scope, &backing);
        let key = deno_core::v8::String::new(scope, "__snowglassBusBuffer")
            .ok_or_else(|| "cannot allocate the background shared-bus property".to_owned())?;
        if !scope
            .get_current_context()
            .global(scope)
            .set(scope, key.into(), buffer.into())
            .unwrap_or(false)
        {
            return Err("cannot install the background shared-bus buffer".to_owned());
        }
        Ok(())
    }
}

struct BackgroundBackingOwner {
    _buffer: ICoreWebView2SharedBuffer,
}

unsafe extern "C" fn release_background_backing(
    _data: *mut c_void,
    _byte_length: usize,
    owner: *mut c_void,
) {
    // SAFETY: `owner` came from one `Box::into_raw` above and V8 invokes the
    // backing-store deleter exactly once.
    drop(unsafe { Box::from_raw(owner.cast::<BackgroundBackingOwner>()) });
}

impl SharedBus {
    pub fn new(webview: &WebView) -> windows::core::Result<Self> {
        let environment: ICoreWebView2Environment12 = webview.environment().cast()?;
        // SAFETY: WebView2 owns the returned mapping for the lifetime of the
        // shared-buffer COM object stored in `Self`.
        let buffer = unsafe { environment.CreateSharedBuffer(ALLOCATION_LEN as u64)? };
        let mut memory = std::ptr::null_mut();
        // SAFETY: `Buffer` initializes the pointer and the allocation is nonzero.
        unsafe { buffer.Buffer(&mut memory)? };
        let memory = NonNull::new(memory).ok_or_else(windows::core::Error::from_win32)?;
        let mut bus = Self {
            buffer,
            memory,
            generation: 1,
        };
        bus.initialize_lanes();
        Ok(bus)
    }

    pub fn background_endpoint(
        &self,
        wake_native: impl Fn() + Send + Sync + 'static,
    ) -> BackgroundBusEndpoint {
        BackgroundBusEndpoint {
            owner: self.buffer.clone(),
            memory: self.memory,
            wake_native: Arc::new(wake_native),
        }
    }

    /// Invalidates every outstanding request without replacing the allocation.
    pub fn next_generation(&mut self) -> u32 {
        self.generation = self.generation.wrapping_add(1).max(1);
        self.initialize_lanes();
        self.generation
    }

    pub const fn generation(&self) -> u32 {
        self.generation
    }

    pub fn attach_foreground(&self, webview: &WebView) -> windows::core::Result<()> {
        let webview: ICoreWebView2_17 = webview.webview().cast()?;
        // SAFETY: The buffer remains owned by `Self`; WebView2 creates a script
        // view whose release is independent of the native view.
        unsafe {
            webview.PostSharedBufferToScript(
                &self.buffer,
                COREWEBVIEW2_SHARED_BUFFER_ACCESS_READ_WRITE,
                PCWSTR::null(),
            )
        }
    }

    pub fn wake_foreground(&self, webview: &WebView) -> windows::core::Result<()> {
        // The string is intentionally empty: this is a doorbell, not transport.
        unsafe {
            webview
                .webview()
                .PostWebMessageAsString(windows::core::w!(""))
        }
    }

    pub fn try_pop(&self, lane: usize) -> Result<Option<BusRecord>, FifoError> {
        let header = self.validate_lane(lane)?;
        let read = unsafe { (&*header.read).load(Ordering::Relaxed) };
        let write = unsafe { (&*header.write).load(Ordering::Acquire) };
        let used = self.validate_cursors(header, read, write)?;
        if used == 0 {
            return Ok(None);
        }

        if used < size_of::<u32>() as u64 {
            return self.fault(header, FifoError::CorruptRecord);
        }

        let commit = unsafe { &*self.commit_ptr(header, read) };
        let committed = commit.load(Ordering::Acquire) as usize;
        if committed == 0 {
            return Ok(None);
        }
        if committed + size_of::<u32>() > LANE_CAPACITY {
            return self.fault(header, FifoError::CorruptRecord);
        }
        let record_len = aligned_record_len(committed)?;
        if record_len as u64 > used {
            return self.fault(header, FifoError::UncommittedRecord);
        }
        let body = self.read_ring(header, read + size_of::<u32>() as u64, committed);
        let record = BusRecord::decode(&body).or_else(|error| self.fault(header, error))?;
        commit.store(0, Ordering::Relaxed);
        unsafe { (&*header.read).store(read + record_len as u64, Ordering::Release) };
        Ok(Some(record))
    }

    pub fn try_push(&self, lane: usize, record: &BusRecord) -> Result<(), FifoError> {
        let header = self.validate_lane(lane)?;
        let body = record.encode()?;
        let committed: u32 = body
            .len()
            .try_into()
            .map_err(|_| FifoError::RecordTooLarge)?;
        let record_len = aligned_record_len(body.len())?;
        if record_len > LANE_CAPACITY {
            return Err(FifoError::RecordTooLarge);
        }
        let read = unsafe { (&*header.read).load(Ordering::Acquire) };
        let write = unsafe { (&*header.write).load(Ordering::Relaxed) };
        let used = self.validate_cursors(header, read, write)?;
        if record_len as u64 > LANE_CAPACITY as u64 - used {
            return Err(FifoError::Full);
        }
        let commit = unsafe { &*self.commit_ptr(header, write) };
        commit.store(0, Ordering::Relaxed);
        self.write_ring(header, write + size_of::<u32>() as u64, &body);
        commit.store(committed, Ordering::Release);
        unsafe { (&*header.write).store(write + record_len as u64, Ordering::Release) };
        Ok(())
    }

    fn initialize_lanes(&mut self) {
        for lane in 0..LANE_COUNT {
            let base = self.lane_base(lane);
            // SAFETY: every lane is contained in the WebView2 allocation and
            // all atomic fields are naturally aligned by the 64-byte header.
            unsafe {
                std::ptr::write_bytes(base, 0, LANE_HEADER_LEN + LANE_CAPACITY);
                (base.add(MAGIC_OFFSET) as *mut u32).write(BUS_MAGIC);
                (base.add(VERSION_OFFSET) as *mut u16).write(BUS_VERSION);
                (base.add(LANE_ID_OFFSET) as *mut u16).write(lane as u16);
                (base.add(CAPACITY_OFFSET) as *mut u32).write(LANE_CAPACITY as u32);
                (&*(base.add(READ_OFFSET) as *mut AtomicU64)).store(0, Ordering::Release);
                (&*(base.add(WRITE_OFFSET) as *mut AtomicU64)).store(0, Ordering::Release);
                (&*(base.add(FAULT_OFFSET) as *mut AtomicU32)).store(0, Ordering::Release);
                (&*(base.add(GENERATION_OFFSET) as *mut AtomicU32))
                    .store(self.generation, Ordering::Release);
            }
        }
    }

    fn validate_lane(&self, lane: usize) -> Result<LanePointers, FifoError> {
        if lane >= LANE_COUNT {
            return Err(FifoError::CorruptHeader);
        }
        let base = self.lane_base(lane);
        // SAFETY: fixed fields fit in the aligned lane header.
        let valid = unsafe {
            (base.add(MAGIC_OFFSET) as *const u32).read() == BUS_MAGIC
                && (base.add(VERSION_OFFSET) as *const u16).read() == BUS_VERSION
                && (base.add(LANE_ID_OFFSET) as *const u16).read() == lane as u16
                && (base.add(CAPACITY_OFFSET) as *const u32).read() == LANE_CAPACITY as u32
                && (&*(base.add(GENERATION_OFFSET) as *const AtomicU32)).load(Ordering::Acquire)
                    == self.generation
        };
        let header = LanePointers {
            data: unsafe { base.add(LANE_HEADER_LEN) },
            read: unsafe { base.add(READ_OFFSET) as *const AtomicU64 },
            write: unsafe { base.add(WRITE_OFFSET) as *const AtomicU64 },
            fault: unsafe { base.add(FAULT_OFFSET) as *const AtomicU32 },
        };
        if !valid || unsafe { (&*header.fault).load(Ordering::Acquire) } != 0 {
            return Err(FifoError::Faulted);
        }
        Ok(header)
    }

    fn validate_cursors(
        &self,
        header: LanePointers,
        read: u64,
        write: u64,
    ) -> Result<u64, FifoError> {
        let Some(used) = write.checked_sub(read) else {
            return self.fault(header, FifoError::CorruptHeader);
        };
        if used > LANE_CAPACITY as u64 {
            return self.fault(header, FifoError::CorruptHeader);
        }
        Ok(used)
    }

    fn fault<T>(&self, header: LanePointers, error: FifoError) -> Result<T, FifoError> {
        unsafe { (&*header.fault).store(error_code(&error), Ordering::Release) };
        Err(error)
    }

    fn lane_base(&self, lane: usize) -> *mut u8 {
        unsafe {
            self.memory
                .as_ptr()
                .add(lane * (LANE_HEADER_LEN + LANE_CAPACITY))
        }
    }

    unsafe fn commit_ptr(&self, header: LanePointers, cursor: u64) -> *const AtomicU32 {
        unsafe { header.data.add(cursor as usize % LANE_CAPACITY) as *const AtomicU32 }
    }

    fn read_ring(&self, header: LanePointers, cursor: u64, length: usize) -> Vec<u8> {
        (0..length)
            .map(|offset| unsafe {
                header
                    .data
                    .add((cursor as usize + offset) % LANE_CAPACITY)
                    .read_volatile()
            })
            .collect()
    }

    fn write_ring(&self, header: LanePointers, cursor: u64, bytes: &[u8]) {
        for (offset, byte) in bytes.iter().enumerate() {
            unsafe {
                header
                    .data
                    .add((cursor as usize + offset) % LANE_CAPACITY)
                    .write_volatile(*byte)
            };
        }
    }
}

impl Drop for SharedBus {
    fn drop(&mut self) {
        // SAFETY: this is the final native owner. Script views have independent
        // lifetime and are explicitly released by the installed client.
        let _ = unsafe { self.buffer.Close() };
    }
}

#[derive(Clone, Copy)]
struct LanePointers {
    data: *mut u8,
    read: *const AtomicU64,
    write: *const AtomicU64,
    fault: *const AtomicU32,
}

fn error_code(error: &FifoError) -> u32 {
    match error {
        FifoError::InvalidCapacity => 1,
        FifoError::RecordTooLarge => 2,
        FifoError::Full => 3,
        FifoError::CorruptHeader => 4,
        FifoError::CorruptRecord => 5,
        FifoError::UncommittedRecord => 6,
        FifoError::InvalidRequestId => 7,
        FifoError::Faulted => 8,
    }
}

fn aligned_record_len(body_len: usize) -> Result<usize, FifoError> {
    body_len
        .checked_add(size_of::<u32>())
        .and_then(|length| length.checked_add(7))
        .map(|length| length & !7)
        .ok_or(FifoError::RecordTooLarge)
}

pub fn encode_json(value: &JsonValue) -> Result<Vec<u8>, String> {
    StructuredValue::from_json(value)
        .map_err(|error| error.to_string())?
        .encode()
        .map_err(|error| error.to_string())
}

pub(crate) struct PluginBusRequest {
    pub generation: u32,
    pub request_id: u64,
    pub response_lane: usize,
    pub plugin: String,
    pub path: Vec<String>,
    pub encoded_arguments: Vec<u8>,
    pub cancellation: snowglass_pluginhost::PluginCancellationToken,
}

pub(crate) enum SharedBusRequest {
    Plugin(PluginBusRequest),
    ShellCommand(JsonValue),
}

/// Native routing, correlation, backpressure, and cancellation state for the
/// shared allocation. The shell coordinator only rings the platform-specific
/// doorbells after this broker drains or flushes lanes.
pub(crate) struct SharedBusBroker {
    pending: [VecDeque<BusRecord>; 2],
    active_plugin_calls: HashMap<(usize, u64), snowglass_pluginhost::PluginCancellationToken>,
    next_native_request: u64,
}

impl Default for SharedBusBroker {
    fn default() -> Self {
        Self {
            pending: std::array::from_fn(|_| VecDeque::new()),
            active_plugin_calls: HashMap::new(),
            next_native_request: 1,
        }
    }
}

impl SharedBusBroker {
    pub fn reset(&mut self) {
        for pending in &mut self.pending {
            pending.clear();
        }
        self.active_plugin_calls.clear();
    }

    pub fn drain_requests(
        &mut self,
        bus: &mut SharedBus,
        runtime: &snowglass_pluginhost::PluginRuntimeHandle,
    ) -> Vec<SharedBusRequest> {
        let mut requests = Vec::new();
        for (request_lane, response_lane, source) in [
            (FOREGROUND_TO_NATIVE, NATIVE_TO_FOREGROUND, "foreground"),
            (BACKGROUND_TO_NATIVE, NATIVE_TO_BACKGROUND, "background"),
        ] {
            loop {
                let record = match bus.try_pop(request_lane) {
                    Ok(Some(record)) => record,
                    Ok(None) => break,
                    Err(error) => {
                        tracing::error!("snowglass: {source} shared-bus lane fault: {error}");
                        break;
                    }
                };
                let forwarded_lane = match (request_lane, record.target) {
                    (FOREGROUND_TO_NATIVE, TARGET_BACKGROUND) => Some(NATIVE_TO_BACKGROUND),
                    (BACKGROUND_TO_NATIVE, TARGET_FOREGROUND) => Some(NATIVE_TO_FOREGROUND),
                    _ => None,
                };
                if let Some(forwarded_lane) = forwarded_lane {
                    if !matches!(record.kind, RECORD_BRIDGE_MESSAGE | RECORD_BRIDGE_PROXY) {
                        tracing::error!(
                            "snowglass: unsupported DOM bridge record kind {}",
                            record.kind
                        );
                        continue;
                    }
                    self.pending[response_queue_index(forwarded_lane)].push_back(record);
                    continue;
                }
                if request_lane == FOREGROUND_TO_NATIVE
                    && record.target == TARGET_NATIVE
                    && record.kind == RECORD_SHELL_COMMAND
                {
                    if !record.export_path.is_empty() {
                        tracing::error!("snowglass: shell command record has an invalid path");
                        continue;
                    }
                    match StructuredValue::decode(&record.payload).and_then(|value| value.to_json())
                    {
                        Ok(command) => requests.push(SharedBusRequest::ShellCommand(command)),
                        Err(error) => {
                            tracing::error!("snowglass: invalid foreground shell command: {error}")
                        }
                    }
                    continue;
                }
                if record.target != TARGET_PLUGIN {
                    tracing::error!("snowglass: unsupported {source} shared-bus route");
                    continue;
                }
                if record.kind == RECORD_CANCEL {
                    if let Some(cancellation) = self
                        .active_plugin_calls
                        .get(&(response_lane, record.request_id))
                    {
                        cancellation.cancel();
                    }
                    continue;
                }
                if record.kind != RECORD_PLUGIN_INVOKE {
                    tracing::error!(
                        "snowglass: unsupported {source} shared-bus record kind {}",
                        record.kind
                    );
                    continue;
                }
                let mut export = record.export_path.split('\0');
                let Some(plugin) = export.next().filter(|plugin| !plugin.is_empty()) else {
                    self.queue_protocol_error(
                        record.request_id,
                        response_lane,
                        "plugin invocation is missing a plugin ID",
                    );
                    continue;
                };
                let path = export.map(str::to_owned).collect::<Vec<_>>();
                if path.is_empty() || path.iter().any(String::is_empty) {
                    self.queue_protocol_error(
                        record.request_id,
                        response_lane,
                        "plugin invocation has an invalid export path",
                    );
                    continue;
                }
                match StructuredValue::decode(&record.payload) {
                    Ok(arguments) if arguments.is_array() => {
                        let key = (response_lane, record.request_id);
                        if self.active_plugin_calls.contains_key(&key) {
                            self.queue_protocol_error(
                                record.request_id,
                                response_lane,
                                "plugin request ID is already active",
                            );
                            continue;
                        }
                        let cancellation = runtime.cancellation_token();
                        self.active_plugin_calls.insert(key, cancellation.clone());
                        requests.push(SharedBusRequest::Plugin(PluginBusRequest {
                            generation: bus.generation(),
                            request_id: record.request_id,
                            response_lane,
                            plugin: plugin.to_owned(),
                            path,
                            encoded_arguments: record.payload,
                            cancellation,
                        }));
                    }
                    Ok(_) => self.queue_protocol_error(
                        record.request_id,
                        response_lane,
                        "plugin invocation payload must be an argument array",
                    ),
                    Err(error) => self.queue_protocol_error(
                        record.request_id,
                        response_lane,
                        &error.to_string(),
                    ),
                }
            }
        }
        self.flush(bus);
        requests
    }

    pub fn queue_foreground_command(&mut self, command: &JsonValue) -> Result<(), String> {
        let request_id = self.next_native_request;
        self.next_native_request = self.next_native_request.wrapping_add(1).max(1);
        self.pending[response_queue_index(NATIVE_TO_FOREGROUND)].push_back(BusRecord {
            kind: RECORD_SHELL_COMMAND,
            flags: 0,
            request_id,
            target: TARGET_FOREGROUND,
            export_path: String::new(),
            payload: encode_json(command)?,
        });
        Ok(())
    }

    pub fn settle_plugin(
        &mut self,
        bus: &SharedBus,
        generation: u32,
        request_id: u64,
        response_lane: usize,
        result: Result<Vec<u8>, snowglass_pluginhost::PluginInvocationError>,
    ) -> bool {
        self.active_plugin_calls
            .remove(&(response_lane, request_id));
        if generation != bus.generation() {
            return false;
        }
        let (kind, payload) = match result {
            Ok(payload) => (RECORD_RESOLVE, payload),
            Err(error) => {
                let value = serde_json::to_value(error).unwrap_or_else(|error| {
                    serde_json::json!({
                        "name": "Error",
                        "code": "PLUGIN_WORKER_FAILED",
                        "message": error.to_string(),
                    })
                });
                (
                    RECORD_REJECT,
                    encode_json(&value).expect("plugin errors are structured-clone encodable"),
                )
            }
        };
        self.pending[response_queue_index(response_lane)].push_back(BusRecord {
            kind,
            flags: 0,
            request_id,
            target: TARGET_PLUGIN,
            export_path: String::new(),
            payload,
        });
        response_lane == NATIVE_TO_FOREGROUND
    }

    pub fn flush(&mut self, bus: &mut SharedBus) {
        for (queue, lane) in [NATIVE_TO_FOREGROUND, NATIVE_TO_BACKGROUND]
            .into_iter()
            .enumerate()
        {
            while let Some(record) = self.pending[queue].front() {
                match bus.try_push(lane, record) {
                    Ok(()) => {
                        self.pending[queue].pop_front();
                    }
                    Err(FifoError::Full) => break,
                    Err(error) => {
                        tracing::error!(
                            "snowglass: native shared-bus response lane fault: {error}"
                        );
                        self.pending[queue].clear();
                        break;
                    }
                }
            }
        }
    }

    fn queue_protocol_error(&mut self, request_id: u64, response_lane: usize, message: &str) {
        let error = serde_json::json!({
            "name": "TypeError",
            "code": "INVALID_ARGUMENTS",
            "message": message,
        });
        self.pending[response_queue_index(response_lane)].push_back(BusRecord {
            kind: RECORD_REJECT,
            flags: 0,
            request_id,
            target: TARGET_PLUGIN,
            export_path: String::new(),
            payload: encode_json(&error).unwrap_or_default(),
        });
    }
}

fn response_queue_index(lane: usize) -> usize {
    match lane {
        NATIVE_TO_FOREGROUND => 0,
        NATIVE_TO_BACKGROUND => 1,
        _ => unreachable!("only native response lanes are queued"),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn json_plugin_arguments_use_the_binary_structured_codec() {
        let arguments = serde_json::json!([
            { "console": "wiiu", "enabled": true },
            [1.5, null, "three"]
        ]);
        let encoded = encode_json(&arguments).unwrap();
        assert_eq!(
            StructuredValue::decode(&encoded)
                .unwrap()
                .to_json()
                .unwrap(),
            arguments
        );
        assert_ne!(encoded.first(), Some(&b'['));
    }

    #[test]
    fn native_foreground_commands_are_binary_fifo_records() {
        let command = serde_json::json!({
            "type": "launcherState",
            "state": { "kind": "idle" },
        });
        let mut broker = SharedBusBroker::default();
        broker.queue_foreground_command(&command).unwrap();
        let record = broker.pending[0].pop_front().unwrap();

        assert_eq!(record.kind, RECORD_SHELL_COMMAND);
        assert_eq!(record.target, TARGET_FOREGROUND);
        assert_eq!(
            StructuredValue::decode(&record.payload)
                .unwrap()
                .to_json()
                .unwrap(),
            command
        );
        assert_ne!(record.payload.first(), Some(&b'{'));
    }

    #[test]
    fn generated_foreground_bridge_uses_payload_free_webview_doorbells() {
        let bridge = include_str!(concat!(env!("OUT_DIR"), "/bridge.js"));
        assert!(!bridge.contains("JSON.stringify"));
        assert!(!bridge.contains("JSON.parse"));
        assert!(!bridge.contains("ipc.postMessage(JSON"));
    }
}
