//! Safe, platform-neutral core of the four-lane shared plugin/bridge bus.
//!
//! WebView2 mapping and V8 external-memory setup deliberately live in the
//! shell adapter. This module defines the binary records, structured values,
//! validation, and lock-free SPSC cursor protocol shared by those adapters.

use std::{
    collections::HashSet,
    fmt,
    sync::{
        Arc,
        atomic::{AtomicU8, AtomicU32, AtomicU64, Ordering},
    },
};
use thiserror::Error;
use tokio::sync::Notify;

pub const BUS_MAGIC: u32 = u32::from_le_bytes(*b"SGB1");
pub const BUS_VERSION: u16 = 1;
const CODEC_MAGIC: u32 = u32::from_le_bytes(*b"SGC1");
const RECORD_HEADER_LEN: usize = 2 + 2 + 8 + 1 + 2 + 4;

#[derive(Clone, Debug, PartialEq)]
pub struct StructuredValue {
    pub root: Value,
    pub nodes: Vec<Node>,
}

#[derive(Clone, Debug, PartialEq)]
pub enum Value {
    Undefined,
    Null,
    Bool(bool),
    Number(f64),
    BigInt(String),
    String(String),
    Ref(u32),
}

#[derive(Clone, Debug, PartialEq)]
pub enum Node {
    Array(Vec<Value>),
    Object(Vec<(String, Value)>),
    Date(f64),
    Map(Vec<(Value, Value)>),
    Set(Vec<Value>),
    Error {
        name: String,
        message: String,
        stack: Option<String>,
        cause: Option<Value>,
    },
    ArrayBuffer(Vec<u8>),
    TypedArray {
        kind: TypedArrayKind,
        bytes: Vec<u8>,
    },
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
#[repr(u8)]
pub enum TypedArrayKind {
    Int8 = 1,
    Uint8 = 2,
    Uint8Clamped = 3,
    Int16 = 4,
    Uint16 = 5,
    Int32 = 6,
    Uint32 = 7,
    Float32 = 8,
    Float64 = 9,
    BigInt64 = 10,
    BigUint64 = 11,
    DataView = 12,
}

impl TryFrom<u8> for TypedArrayKind {
    type Error = CodecError;

    fn try_from(value: u8) -> Result<Self, Self::Error> {
        Ok(match value {
            1 => Self::Int8,
            2 => Self::Uint8,
            3 => Self::Uint8Clamped,
            4 => Self::Int16,
            5 => Self::Uint16,
            6 => Self::Int32,
            7 => Self::Uint32,
            8 => Self::Float32,
            9 => Self::Float64,
            10 => Self::BigInt64,
            11 => Self::BigUint64,
            12 => Self::DataView,
            tag => return Err(CodecError::InvalidTag(tag)),
        })
    }
}

impl StructuredValue {
    pub fn from_json(value: &serde_json::Value) -> Result<Self, CodecError> {
        fn convert(value: &serde_json::Value, nodes: &mut Vec<Node>) -> Result<Value, CodecError> {
            Ok(match value {
                serde_json::Value::Null => Value::Null,
                serde_json::Value::Bool(value) => Value::Bool(*value),
                serde_json::Value::Number(value) => {
                    Value::Number(value.as_f64().ok_or(CodecError::UnsupportedJsonNumber)?)
                }
                serde_json::Value::String(value) => Value::String(value.clone()),
                serde_json::Value::Array(values) => {
                    let index = usize_u32(nodes.len())?;
                    nodes.push(Node::Array(Vec::new()));
                    let values = values
                        .iter()
                        .map(|value| convert(value, nodes))
                        .collect::<Result<_, _>>()?;
                    nodes[index as usize] = Node::Array(values);
                    Value::Ref(index)
                }
                serde_json::Value::Object(values) => {
                    let index = usize_u32(nodes.len())?;
                    nodes.push(Node::Object(Vec::new()));
                    let values = values
                        .iter()
                        .map(|(key, value)| Ok((key.clone(), convert(value, nodes)?)))
                        .collect::<Result<_, CodecError>>()?;
                    nodes[index as usize] = Node::Object(values);
                    Value::Ref(index)
                }
            })
        }

        let mut nodes = Vec::new();
        let root = convert(value, &mut nodes)?;
        Ok(Self { root, nodes })
    }

    pub fn to_json(&self) -> Result<serde_json::Value, CodecError> {
        fn materialize(
            value: &Value,
            nodes: &[Node],
            ancestors: &mut HashSet<u32>,
        ) -> Result<serde_json::Value, CodecError> {
            Ok(match value {
                Value::Null => serde_json::Value::Null,
                Value::Bool(value) => serde_json::Value::Bool(*value),
                Value::Number(value) => json_number(*value)
                    .map(serde_json::Value::Number)
                    .ok_or(CodecError::UnsupportedJsonNumber)?,
                Value::String(value) => serde_json::Value::String(value.clone()),
                Value::Undefined | Value::BigInt(_) => return Err(CodecError::NotJsonCompatible),
                Value::Ref(index) => {
                    if !ancestors.insert(*index) {
                        return Err(CodecError::NotJsonCompatible);
                    }
                    let node = nodes
                        .get(*index as usize)
                        .ok_or(CodecError::InvalidReference(*index))?;
                    let result = match node {
                        Node::Array(values) => serde_json::Value::Array(
                            values
                                .iter()
                                .map(|value| materialize(value, nodes, ancestors))
                                .collect::<Result<_, _>>()?,
                        ),
                        Node::Object(entries) => serde_json::Value::Object(
                            entries
                                .iter()
                                .map(|(key, value)| {
                                    Ok((key.clone(), materialize(value, nodes, ancestors)?))
                                })
                                .collect::<Result<_, CodecError>>()?,
                        ),
                        _ => return Err(CodecError::NotJsonCompatible),
                    };
                    ancestors.remove(index);
                    result
                }
            })
        }

        materialize(&self.root, &self.nodes, &mut HashSet::new())
    }

    pub fn is_array(&self) -> bool {
        matches!(
            self.root,
            Value::Ref(index) if matches!(self.nodes.get(index as usize), Some(Node::Array(_)))
        )
    }

    pub fn encode(&self) -> Result<Vec<u8>, CodecError> {
        validate_value(&self.root, self.nodes.len())?;
        let mut out = Vec::new();
        put_u32(&mut out, CODEC_MAGIC);
        put_u16(&mut out, BUS_VERSION);
        put_u32(&mut out, usize_u32(self.nodes.len())?);
        encode_value(&mut out, &self.root)?;
        for node in &self.nodes {
            encode_node(&mut out, node, self.nodes.len())?;
        }
        Ok(out)
    }

    pub fn decode(bytes: &[u8]) -> Result<Self, CodecError> {
        let mut input = Decoder::new(bytes);
        if input.u32()? != CODEC_MAGIC {
            return Err(CodecError::BadMagic);
        }
        let version = input.u16()?;
        if version != BUS_VERSION {
            return Err(CodecError::UnsupportedVersion(version));
        }
        let node_count = input.u32()? as usize;
        if node_count > bytes.len() {
            return Err(CodecError::InvalidLength);
        }
        let root = input.value()?;
        let mut nodes = Vec::with_capacity(node_count);
        for _ in 0..node_count {
            nodes.push(input.node()?);
        }
        if !input.remaining().is_empty() {
            return Err(CodecError::TrailingData);
        }
        validate_value(&root, nodes.len())?;
        for node in &nodes {
            validate_node(node, nodes.len())?;
        }
        Ok(Self { root, nodes })
    }
}

/// Restores the integer representation that JSON parsing would assign to a
/// JavaScript safe integer. The structured-clone wire format correctly has one
/// `Number` variant because JavaScript itself does, but native command DTOs use
/// integer fields and Serde intentionally rejects an `f64` such as `1.0` for
/// those fields.
fn json_number(value: f64) -> Option<serde_json::Number> {
    const MAX_SAFE_INTEGER: f64 = 9_007_199_254_740_991.0;

    if value.fract() == 0.0 && value.abs() <= MAX_SAFE_INTEGER {
        if value.is_sign_negative() {
            return Some((value as i64).into());
        }
        return Some((value as u64).into());
    }
    serde_json::Number::from_f64(value)
}

fn validate_value(value: &Value, nodes: usize) -> Result<(), CodecError> {
    match value {
        Value::Ref(index) if *index as usize >= nodes => Err(CodecError::InvalidReference(*index)),
        Value::BigInt(value)
            if value.is_empty()
                || value == "-"
                || !value
                    .bytes()
                    .enumerate()
                    .all(|(index, byte)| byte.is_ascii_digit() || (index == 0 && byte == b'-')) =>
        {
            Err(CodecError::InvalidBigInt)
        }
        _ => Ok(()),
    }
}

fn validate_node(node: &Node, nodes: usize) -> Result<(), CodecError> {
    match node {
        Node::Array(values) | Node::Set(values) => {
            for value in values {
                validate_value(value, nodes)?;
            }
        }
        Node::Object(entries) => {
            for (_, value) in entries {
                validate_value(value, nodes)?;
            }
        }
        Node::Map(entries) => {
            for (key, value) in entries {
                validate_value(key, nodes)?;
                validate_value(value, nodes)?;
            }
        }
        Node::Error { cause, .. } => {
            if let Some(cause) = cause {
                validate_value(cause, nodes)?;
            }
        }
        Node::Date(_) | Node::ArrayBuffer(_) | Node::TypedArray { .. } => {}
    }
    Ok(())
}

fn encode_value(out: &mut Vec<u8>, value: &Value) -> Result<(), CodecError> {
    match value {
        Value::Undefined => out.push(0),
        Value::Null => out.push(1),
        Value::Bool(false) => out.push(2),
        Value::Bool(true) => out.push(3),
        Value::Number(value) => {
            out.push(4);
            out.extend_from_slice(&value.to_bits().to_le_bytes());
        }
        Value::BigInt(value) => {
            out.push(5);
            put_string(out, value)?;
        }
        Value::String(value) => {
            out.push(6);
            put_string(out, value)?;
        }
        Value::Ref(value) => {
            out.push(7);
            put_u32(out, *value);
        }
    }
    Ok(())
}

fn encode_node(out: &mut Vec<u8>, node: &Node, nodes: usize) -> Result<(), CodecError> {
    validate_node(node, nodes)?;
    match node {
        Node::Array(values) => {
            out.push(1);
            put_values(out, values)?;
        }
        Node::Object(entries) => {
            out.push(2);
            put_u32(out, usize_u32(entries.len())?);
            for (key, value) in entries {
                put_string(out, key)?;
                encode_value(out, value)?;
            }
        }
        Node::Date(value) => {
            out.push(3);
            out.extend_from_slice(&value.to_bits().to_le_bytes());
        }
        Node::Map(entries) => {
            out.push(4);
            put_u32(out, usize_u32(entries.len())?);
            for (key, value) in entries {
                encode_value(out, key)?;
                encode_value(out, value)?;
            }
        }
        Node::Set(values) => {
            out.push(5);
            put_values(out, values)?;
        }
        Node::Error {
            name,
            message,
            stack,
            cause,
        } => {
            out.push(6);
            put_string(out, name)?;
            put_string(out, message)?;
            put_optional_string(out, stack.as_deref())?;
            out.push(u8::from(cause.is_some()));
            if let Some(cause) = cause {
                encode_value(out, cause)?;
            }
        }
        Node::ArrayBuffer(bytes) => {
            out.push(7);
            put_bytes(out, bytes)?;
        }
        Node::TypedArray { kind, bytes } => {
            out.push(8);
            out.push(*kind as u8);
            put_bytes(out, bytes)?;
        }
    }
    Ok(())
}

fn put_values(out: &mut Vec<u8>, values: &[Value]) -> Result<(), CodecError> {
    put_u32(out, usize_u32(values.len())?);
    for value in values {
        encode_value(out, value)?;
    }
    Ok(())
}

fn put_optional_string(out: &mut Vec<u8>, value: Option<&str>) -> Result<(), CodecError> {
    out.push(u8::from(value.is_some()));
    if let Some(value) = value {
        put_string(out, value)?;
    }
    Ok(())
}

fn put_string(out: &mut Vec<u8>, value: &str) -> Result<(), CodecError> {
    put_bytes(out, value.as_bytes())
}

fn put_bytes(out: &mut Vec<u8>, bytes: &[u8]) -> Result<(), CodecError> {
    put_u32(out, usize_u32(bytes.len())?);
    out.extend_from_slice(bytes);
    Ok(())
}

fn put_u16(out: &mut Vec<u8>, value: u16) {
    out.extend_from_slice(&value.to_le_bytes());
}

fn put_u32(out: &mut Vec<u8>, value: u32) {
    out.extend_from_slice(&value.to_le_bytes());
}

fn usize_u32(value: usize) -> Result<u32, CodecError> {
    value.try_into().map_err(|_| CodecError::InvalidLength)
}

struct Decoder<'a> {
    input: &'a [u8],
    cursor: usize,
}

impl<'a> Decoder<'a> {
    fn new(input: &'a [u8]) -> Self {
        Self { input, cursor: 0 }
    }

    fn remaining(&self) -> &'a [u8] {
        &self.input[self.cursor..]
    }

    fn take(&mut self, length: usize) -> Result<&'a [u8], CodecError> {
        let end = self
            .cursor
            .checked_add(length)
            .filter(|end| *end <= self.input.len())
            .ok_or(CodecError::UnexpectedEnd)?;
        let result = &self.input[self.cursor..end];
        self.cursor = end;
        Ok(result)
    }

    fn byte(&mut self) -> Result<u8, CodecError> {
        Ok(self.take(1)?[0])
    }

    fn u16(&mut self) -> Result<u16, CodecError> {
        Ok(u16::from_le_bytes(self.take(2)?.try_into().unwrap()))
    }

    fn u32(&mut self) -> Result<u32, CodecError> {
        Ok(u32::from_le_bytes(self.take(4)?.try_into().unwrap()))
    }

    fn f64(&mut self) -> Result<f64, CodecError> {
        Ok(f64::from_bits(u64::from_le_bytes(
            self.take(8)?.try_into().unwrap(),
        )))
    }

    fn bytes(&mut self) -> Result<Vec<u8>, CodecError> {
        let length = self.u32()? as usize;
        Ok(self.take(length)?.to_vec())
    }

    fn string(&mut self) -> Result<String, CodecError> {
        String::from_utf8(self.bytes()?).map_err(|_| CodecError::InvalidUtf8)
    }

    fn value(&mut self) -> Result<Value, CodecError> {
        Ok(match self.byte()? {
            0 => Value::Undefined,
            1 => Value::Null,
            2 => Value::Bool(false),
            3 => Value::Bool(true),
            4 => Value::Number(self.f64()?),
            5 => Value::BigInt(self.string()?),
            6 => Value::String(self.string()?),
            7 => Value::Ref(self.u32()?),
            tag => return Err(CodecError::InvalidTag(tag)),
        })
    }

    fn values(&mut self) -> Result<Vec<Value>, CodecError> {
        let length = self.u32()? as usize;
        if length > self.remaining().len() {
            return Err(CodecError::InvalidLength);
        }
        (0..length).map(|_| self.value()).collect()
    }

    fn optional_string(&mut self) -> Result<Option<String>, CodecError> {
        match self.byte()? {
            0 => Ok(None),
            1 => Ok(Some(self.string()?)),
            tag => Err(CodecError::InvalidTag(tag)),
        }
    }

    fn node(&mut self) -> Result<Node, CodecError> {
        Ok(match self.byte()? {
            1 => Node::Array(self.values()?),
            2 => {
                let length = self.u32()? as usize;
                if length > self.remaining().len() {
                    return Err(CodecError::InvalidLength);
                }
                Node::Object(
                    (0..length)
                        .map(|_| Ok((self.string()?, self.value()?)))
                        .collect::<Result<_, CodecError>>()?,
                )
            }
            3 => Node::Date(self.f64()?),
            4 => {
                let length = self.u32()? as usize;
                if length > self.remaining().len() {
                    return Err(CodecError::InvalidLength);
                }
                Node::Map(
                    (0..length)
                        .map(|_| Ok((self.value()?, self.value()?)))
                        .collect::<Result<_, CodecError>>()?,
                )
            }
            5 => Node::Set(self.values()?),
            6 => Node::Error {
                name: self.string()?,
                message: self.string()?,
                stack: self.optional_string()?,
                cause: match self.byte()? {
                    0 => None,
                    1 => Some(self.value()?),
                    tag => return Err(CodecError::InvalidTag(tag)),
                },
            },
            7 => Node::ArrayBuffer(self.bytes()?),
            8 => Node::TypedArray {
                kind: self.byte()?.try_into()?,
                bytes: self.bytes()?,
            },
            tag => return Err(CodecError::InvalidTag(tag)),
        })
    }
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct BusRecord {
    pub kind: u16,
    pub flags: u16,
    pub request_id: u64,
    pub target: u8,
    pub export_path: String,
    pub payload: Vec<u8>,
}

impl BusRecord {
    /// Encodes the record body used after a lane's committed-length word.
    ///
    /// Platform adapters use this to put the same validated wire format in
    /// externally mapped WebView/V8 memory without depending on this module's
    /// in-process storage implementation.
    pub fn encode(&self) -> Result<Vec<u8>, FifoError> {
        if self.request_id == 0 {
            return Err(FifoError::InvalidRequestId);
        }
        let path = self.export_path.as_bytes();
        let path_length: u16 = path
            .len()
            .try_into()
            .map_err(|_| FifoError::RecordTooLarge)?;
        let payload_length: u32 = self
            .payload
            .len()
            .try_into()
            .map_err(|_| FifoError::RecordTooLarge)?;
        let mut out = Vec::with_capacity(RECORD_HEADER_LEN + path.len() + self.payload.len());
        put_u16(&mut out, self.kind);
        put_u16(&mut out, self.flags);
        out.extend_from_slice(&self.request_id.to_le_bytes());
        out.push(self.target);
        put_u16(&mut out, path_length);
        put_u32(&mut out, payload_length);
        out.extend_from_slice(path);
        out.extend_from_slice(&self.payload);
        Ok(out)
    }

    /// Decodes and validates a record body read from any bus lane.
    pub fn decode(bytes: &[u8]) -> Result<Self, FifoError> {
        if bytes.len() < RECORD_HEADER_LEN {
            return Err(FifoError::CorruptRecord);
        }
        let kind = u16::from_le_bytes(bytes[0..2].try_into().unwrap());
        let flags = u16::from_le_bytes(bytes[2..4].try_into().unwrap());
        let request_id = u64::from_le_bytes(bytes[4..12].try_into().unwrap());
        if request_id == 0 {
            return Err(FifoError::InvalidRequestId);
        }
        let target = bytes[12];
        let path_length = u16::from_le_bytes(bytes[13..15].try_into().unwrap()) as usize;
        let payload_length = u32::from_le_bytes(bytes[15..19].try_into().unwrap()) as usize;
        let expected = RECORD_HEADER_LEN
            .checked_add(path_length)
            .and_then(|length| length.checked_add(payload_length))
            .ok_or(FifoError::CorruptRecord)?;
        if expected != bytes.len() {
            return Err(FifoError::CorruptRecord);
        }
        let path_end = RECORD_HEADER_LEN + path_length;
        let export_path = std::str::from_utf8(&bytes[RECORD_HEADER_LEN..path_end])
            .map_err(|_| FifoError::CorruptRecord)?
            .to_owned();
        Ok(Self {
            kind,
            flags,
            request_id,
            target,
            export_path,
            payload: bytes[path_end..].to_vec(),
        })
    }
}

struct LaneInner {
    capacity: usize,
    generation: AtomicU32,
    read: AtomicU64,
    write: AtomicU64,
    fault: AtomicU32,
    bytes: Box<[AtomicU8]>,
    data: Notify,
    space: Notify,
}

#[derive(Clone)]
pub struct FifoLane(Arc<LaneInner>);

impl fmt::Debug for FifoLane {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter
            .debug_struct("FifoLane")
            .field("header", &self.header())
            .finish()
    }
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct LaneHeader {
    pub magic: u32,
    pub version: u16,
    pub generation: u32,
    pub capacity: usize,
    pub read_cursor: u64,
    pub write_cursor: u64,
    pub fault: u32,
}

impl FifoLane {
    pub fn new(capacity: usize, generation: u32) -> Result<Self, FifoError> {
        if capacity < RECORD_HEADER_LEN + 5 || capacity > u32::MAX as usize {
            return Err(FifoError::InvalidCapacity);
        }
        Ok(Self(Arc::new(LaneInner {
            capacity,
            generation: AtomicU32::new(generation),
            read: AtomicU64::new(0),
            write: AtomicU64::new(0),
            fault: AtomicU32::new(0),
            bytes: (0..capacity).map(|_| AtomicU8::new(0)).collect(),
            data: Notify::new(),
            space: Notify::new(),
        })))
    }

    pub fn header(&self) -> LaneHeader {
        LaneHeader {
            magic: BUS_MAGIC,
            version: BUS_VERSION,
            generation: self.0.generation.load(Ordering::Acquire),
            capacity: self.0.capacity,
            read_cursor: self.0.read.load(Ordering::Acquire),
            write_cursor: self.0.write.load(Ordering::Acquire),
            fault: self.0.fault.load(Ordering::Acquire),
        }
    }

    pub fn reset_generation(&self, generation: u32) {
        self.0.fault.store(0, Ordering::Release);
        self.0.read.store(0, Ordering::Release);
        self.0.write.store(0, Ordering::Release);
        self.0.generation.store(generation, Ordering::Release);
        self.0.data.notify_waiters();
        self.0.space.notify_waiters();
    }

    pub fn try_push(&self, record: &BusRecord) -> Result<(), FifoError> {
        self.ensure_healthy()?;
        let body = record.encode()?;
        let committed: u32 = body
            .len()
            .try_into()
            .map_err(|_| FifoError::RecordTooLarge)?;
        let total = body.len() + 4;
        if total > self.0.capacity {
            return Err(FifoError::RecordTooLarge);
        }
        let read = self.0.read.load(Ordering::Acquire);
        let write = self.0.write.load(Ordering::Relaxed);
        let used = self.validate_cursors(read, write)?;
        if total as u64 > self.0.capacity as u64 - used {
            return Err(FifoError::Full);
        }
        self.write_bytes(write, &0_u32.to_le_bytes(), Ordering::Relaxed);
        self.write_bytes(write + 4, &body, Ordering::Relaxed);
        self.write_bytes(write, &committed.to_le_bytes(), Ordering::Release);
        self.0.write.store(write + total as u64, Ordering::Release);
        self.0.data.notify_one();
        Ok(())
    }

    pub async fn push(&self, record: &BusRecord) -> Result<(), FifoError> {
        loop {
            let notified = self.0.space.notified();
            match self.try_push(record) {
                Err(FifoError::Full) => notified.await,
                result => return result,
            }
        }
    }

    pub fn try_pop(&self) -> Result<Option<BusRecord>, FifoError> {
        self.ensure_healthy()?;
        let read = self.0.read.load(Ordering::Relaxed);
        let write = self.0.write.load(Ordering::Acquire);
        let used = self.validate_cursors(read, write)?;
        if used == 0 {
            return Ok(None);
        }
        if used < 4 {
            return self.fault(FifoError::CorruptRecord);
        }
        let committed = u32::from_le_bytes(self.read_array::<4>(read, Ordering::Acquire)) as usize;
        if committed < RECORD_HEADER_LEN || committed + 4 > self.0.capacity {
            return self.fault(FifoError::CorruptRecord);
        }
        if (committed + 4) as u64 > used {
            return self.fault(FifoError::UncommittedRecord);
        }
        let body = self.read_bytes(read + 4, committed, Ordering::Relaxed);
        let record = BusRecord::decode(&body).or_else(|error| self.fault(error))?;
        self.write_bytes(read, &0_u32.to_le_bytes(), Ordering::Relaxed);
        self.0
            .read
            .store(read + 4 + committed as u64, Ordering::Release);
        self.0.space.notify_one();
        Ok(Some(record))
    }

    pub async fn pop(&self) -> Result<BusRecord, FifoError> {
        loop {
            let notified = self.0.data.notified();
            if let Some(record) = self.try_pop()? {
                return Ok(record);
            }
            notified.await;
        }
    }

    fn validate_cursors(&self, read: u64, write: u64) -> Result<u64, FifoError> {
        let Some(used) = write.checked_sub(read) else {
            return self.fault(FifoError::CorruptHeader);
        };
        if used > self.0.capacity as u64 {
            return self.fault(FifoError::CorruptHeader);
        }
        Ok(used)
    }

    fn ensure_healthy(&self) -> Result<(), FifoError> {
        if self.0.fault.load(Ordering::Acquire) != 0 {
            Err(FifoError::Faulted)
        } else {
            Ok(())
        }
    }

    fn fault<T>(&self, error: FifoError) -> Result<T, FifoError> {
        self.0.fault.store(error.code(), Ordering::Release);
        Err(error)
    }

    fn index(&self, cursor: u64) -> usize {
        (cursor % self.0.capacity as u64) as usize
    }

    fn write_bytes(&self, cursor: u64, bytes: &[u8], final_ordering: Ordering) {
        for (offset, byte) in bytes.iter().enumerate() {
            let ordering = if offset + 1 == bytes.len() {
                final_ordering
            } else {
                Ordering::Relaxed
            };
            self.0.bytes[self.index(cursor + offset as u64)].store(*byte, ordering);
        }
    }

    fn read_bytes(&self, cursor: u64, length: usize, first_ordering: Ordering) -> Vec<u8> {
        (0..length)
            .map(|offset| {
                self.0.bytes[self.index(cursor + offset as u64)].load(if offset == 0 {
                    first_ordering
                } else {
                    Ordering::Relaxed
                })
            })
            .collect()
    }

    fn read_array<const N: usize>(&self, cursor: u64, ordering: Ordering) -> [u8; N] {
        // `write_bytes` publishes a multi-byte field by applying Release to
        // its final byte. Acquire that same byte before any relaxed reads of
        // the record body. Acquiring the first byte would not synchronize
        // with the producer on weakly ordered architectures.
        std::array::from_fn(|offset| {
            self.0.bytes[self.index(cursor + offset as u64)].load(if offset + 1 == N {
                ordering
            } else {
                Ordering::Relaxed
            })
        })
    }
}

/// Four independent SPSC lanes over one logical allocation. The platform
/// adapter maps these lanes into the corresponding regions of WebView2's
/// shared allocation.
pub struct SharedFifoBus {
    generation: AtomicU32,
    pub foreground_to_native: FifoLane,
    pub native_to_foreground: FifoLane,
    pub background_to_native: FifoLane,
    pub native_to_background: FifoLane,
}

impl SharedFifoBus {
    pub fn new(lane_capacity: usize) -> Result<Self, FifoError> {
        Ok(Self {
            generation: AtomicU32::new(1),
            foreground_to_native: FifoLane::new(lane_capacity, 1)?,
            native_to_foreground: FifoLane::new(lane_capacity, 1)?,
            background_to_native: FifoLane::new(lane_capacity, 1)?,
            native_to_background: FifoLane::new(lane_capacity, 1)?,
        })
    }

    pub fn generation(&self) -> u32 {
        self.generation.load(Ordering::Acquire)
    }

    pub fn next_generation(&self) -> u32 {
        let generation = self.generation.fetch_add(1, Ordering::AcqRel) + 1;
        for lane in [
            &self.foreground_to_native,
            &self.native_to_foreground,
            &self.background_to_native,
            &self.native_to_background,
        ] {
            lane.reset_generation(generation);
        }
        generation
    }
}

#[derive(Clone, Debug, Eq, Error, PartialEq)]
pub enum CodecError {
    #[error("structured value has the wrong magic")]
    BadMagic,
    #[error("unsupported structured value version {0}")]
    UnsupportedVersion(u16),
    #[error("unexpected end of structured value")]
    UnexpectedEnd,
    #[error("invalid structured value length")]
    InvalidLength,
    #[error("invalid structured value tag {0}")]
    InvalidTag(u8),
    #[error("structured value contains invalid UTF-8")]
    InvalidUtf8,
    #[error("structured value references missing node {0}")]
    InvalidReference(u32),
    #[error("invalid bigint representation")]
    InvalidBigInt,
    #[error("number cannot be represented by the JSON adapter")]
    UnsupportedJsonNumber,
    #[error("structured value is not JSON-compatible")]
    NotJsonCompatible,
    #[error("structured value has trailing data")]
    TrailingData,
}

#[derive(Clone, Debug, Eq, Error, PartialEq)]
pub enum FifoError {
    #[error("invalid FIFO capacity")]
    InvalidCapacity,
    #[error("record exceeds FIFO capacity")]
    RecordTooLarge,
    #[error("FIFO is full")]
    Full,
    #[error("FIFO header is corrupt")]
    CorruptHeader,
    #[error("FIFO record is corrupt")]
    CorruptRecord,
    #[error("FIFO record was visible before commit")]
    UncommittedRecord,
    #[error("request IDs must be non-zero")]
    InvalidRequestId,
    #[error("FIFO is faulted")]
    Faulted,
}

impl FifoError {
    fn code(&self) -> u32 {
        match self {
            Self::InvalidCapacity => 1,
            Self::RecordTooLarge => 2,
            Self::Full => 3,
            Self::CorruptHeader => 4,
            Self::CorruptRecord => 5,
            Self::UncommittedRecord => 6,
            Self::InvalidRequestId => 7,
            Self::Faulted => 8,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn record(id: u64, payload: usize) -> BusRecord {
        BusRecord {
            kind: 3,
            flags: 1,
            request_id: id,
            target: 2,
            export_path: "default.setupLaunch".to_owned(),
            payload: (0..payload).map(|byte| byte as u8).collect(),
        }
    }

    #[test]
    fn codec_round_trips_cycles_binary_and_collection_types() {
        let value = StructuredValue {
            root: Value::Ref(0),
            nodes: vec![
                Node::Object(vec![
                    ("self".to_owned(), Value::Ref(0)),
                    ("map".to_owned(), Value::Ref(1)),
                    ("typed".to_owned(), Value::Ref(3)),
                    (
                        "big".to_owned(),
                        Value::BigInt("-12345678901234567890".to_owned()),
                    ),
                ]),
                Node::Map(vec![(Value::String("date".to_owned()), Value::Ref(2))]),
                Node::Date(1_725_000_000_000.0),
                Node::TypedArray {
                    kind: TypedArrayKind::Uint16,
                    bytes: vec![0, 1, 2, 3],
                },
                Node::Set(vec![Value::Ref(0), Value::Null]),
                Node::Error {
                    name: "Error".to_owned(),
                    message: "failed".to_owned(),
                    stack: Some("stack".to_owned()),
                    cause: Some(Value::Ref(0)),
                },
                Node::ArrayBuffer(vec![0, 255, 1, 254]),
            ],
        };
        assert_eq!(
            StructuredValue::decode(&value.encode().unwrap()).unwrap(),
            value
        );
    }

    #[test]
    fn codec_rejects_malformed_and_dangling_data() {
        let dangling = StructuredValue {
            root: Value::Ref(1),
            nodes: vec![Node::Array(Vec::new())],
        };
        assert_eq!(dangling.encode(), Err(CodecError::InvalidReference(1)));
        assert!(matches!(
            StructuredValue::decode(b"short"),
            Err(CodecError::BadMagic | CodecError::UnexpectedEnd)
        ));
        let mut valid = StructuredValue {
            root: Value::Null,
            nodes: vec![],
        }
        .encode()
        .unwrap();
        valid.push(1);
        assert_eq!(
            StructuredValue::decode(&valid),
            Err(CodecError::TrailingData)
        );
    }

    #[test]
    fn json_adapter_restores_safe_integer_number_kinds() {
        let value = StructuredValue {
            root: Value::Ref(0),
            nodes: vec![Node::Array(vec![
                Value::Number(1.0),
                Value::Number(-2.0),
                Value::Number(2.5),
                Value::Number(9_007_199_254_740_992.0),
            ])],
        };

        let json = value.to_json().unwrap();
        let values = json.as_array().unwrap();
        assert_eq!(values[0].as_u64(), Some(1));
        assert_eq!(values[1].as_i64(), Some(-2));
        assert!(values[2].as_f64().is_some_and(|value| value == 2.5));
        assert!(values[2].as_i64().is_none());
        assert!(values[3].as_u64().is_none());
    }

    #[tokio::test(flavor = "current_thread")]
    async fn fifo_wraps_applies_backpressure_and_preserves_order() {
        let lane = FifoLane::new(128, 1).unwrap();
        let first = record(1, 45);
        let second = record(2, 20);
        lane.push(&first).await.unwrap();
        assert_eq!(lane.try_push(&second), Err(FifoError::Full));
        assert_eq!(lane.pop().await.unwrap(), first);
        lane.push(&second).await.unwrap();
        assert_eq!(lane.pop().await.unwrap(), second);
        assert_eq!(lane.try_pop().unwrap(), None);
        assert!(lane.header().write_cursor > lane.header().capacity as u64);
    }

    #[test]
    fn lanes_progress_independently_and_generation_resets_them() {
        let bus = SharedFifoBus::new(128).unwrap();
        bus.foreground_to_native.try_push(&record(1, 0)).unwrap();
        bus.background_to_native.try_push(&record(2, 0)).unwrap();
        assert_eq!(
            bus.foreground_to_native
                .try_pop()
                .unwrap()
                .unwrap()
                .request_id,
            1
        );
        assert_eq!(bus.background_to_native.header().read_cursor, 0);
        assert_eq!(bus.next_generation(), 2);
        assert_eq!(bus.background_to_native.header().generation, 2);
        assert_eq!(bus.background_to_native.try_pop().unwrap(), None);
    }

    #[test]
    fn simultaneous_foreground_and_background_calls_cannot_cross_deliver() {
        let bus = SharedFifoBus::new(256).unwrap();
        let foreground = BusRecord {
            target: 1,
            export_path: "cemu\0setupLaunch".to_owned(),
            ..record(7, 12)
        };
        let background = BusRecord {
            target: 1,
            export_path: "cemu\0probe".to_owned(),
            ..record(7, 9)
        };

        bus.foreground_to_native.try_push(&foreground).unwrap();
        bus.background_to_native.try_push(&background).unwrap();
        assert_eq!(
            bus.foreground_to_native.try_pop().unwrap(),
            Some(foreground)
        );
        assert_eq!(
            bus.background_to_native.try_pop().unwrap(),
            Some(background)
        );

        let foreground_result = BusRecord {
            kind: 2,
            flags: 0,
            request_id: 7,
            target: 1,
            export_path: String::new(),
            payload: b"foreground".to_vec(),
        };
        let background_result = BusRecord {
            payload: b"background".to_vec(),
            ..foreground_result.clone()
        };
        // Settle in the opposite order to the requests. Correlation is scoped
        // to the response lane, so equal caller-local IDs remain independent.
        bus.native_to_background
            .try_push(&background_result)
            .unwrap();
        bus.native_to_foreground
            .try_push(&foreground_result)
            .unwrap();
        assert_eq!(
            bus.native_to_foreground.try_pop().unwrap(),
            Some(foreground_result)
        );
        assert_eq!(
            bus.native_to_background.try_pop().unwrap(),
            Some(background_result)
        );
    }

    #[test]
    fn invalid_request_ids_never_enter_the_lane() {
        let lane = FifoLane::new(128, 1).unwrap();
        assert_eq!(
            lane.try_push(&record(0, 0)),
            Err(FifoError::InvalidRequestId)
        );
        assert_eq!(lane.header().write_cursor, 0);
    }

    #[test]
    fn record_is_invisible_until_the_write_cursor_is_published() {
        let lane = FifoLane::new(128, 1).unwrap();
        let expected = record(41, 8);
        let body = expected.encode().unwrap();
        let committed = u32::try_from(body.len()).unwrap();

        lane.write_bytes(4, &body, Ordering::Relaxed);
        lane.write_bytes(0, &committed.to_le_bytes(), Ordering::Release);
        assert_eq!(lane.try_pop().unwrap(), None);

        lane.0
            .write
            .store((body.len() + 4) as u64, Ordering::Release);
        assert_eq!(lane.try_pop().unwrap(), Some(expected));
    }

    #[test]
    fn corrupt_cursor_faults_until_a_generation_reset() {
        let lane = FifoLane::new(128, 1).unwrap();
        lane.0.write.store(129, Ordering::Release);

        assert_eq!(lane.try_pop(), Err(FifoError::CorruptHeader));
        assert_eq!(lane.try_pop(), Err(FifoError::Faulted));
        assert_eq!(lane.header().fault, FifoError::CorruptHeader.code());

        lane.reset_generation(2);
        assert_eq!(lane.header().generation, 2);
        assert_eq!(lane.header().fault, 0);
        assert_eq!(lane.try_pop().unwrap(), None);
    }
}
