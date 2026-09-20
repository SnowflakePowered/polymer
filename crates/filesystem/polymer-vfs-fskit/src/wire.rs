use serde::{Deserialize, Serialize, de::DeserializeOwned};
use std::io;

pub(crate) const MAX_IO: usize = 1024 * 1024;
const MAX_FRAME: usize = 8 * MAX_IO;

#[derive(Serialize, Deserialize)]
pub(crate) enum Request {
    Stat {
        path: Vec<u8>,
    },
    List {
        path: Vec<u8>,
    },
    Open {
        path: Vec<u8>,
        read: bool,
        write: bool,
    },
    Read {
        handle: u64,
        offset: u64,
        length: usize,
    },
    Write {
        handle: u64,
        offset: u64,
        data: Vec<u8>,
    },
    Close {
        handle: u64,
    },
    Create {
        path: Vec<u8>,
        directory: bool,
    },
    Remove {
        path: Vec<u8>,
        directory: bool,
    },
    Rename {
        source: Vec<u8>,
        target: Vec<u8>,
        replace: bool,
    },
    SetAttributes {
        path: Vec<u8>,
        size: Option<u64>,
        readonly: Option<bool>,
    },
    Sync,
}

#[derive(Clone, Serialize, Deserialize)]
pub(crate) struct Attributes {
    pub directory: bool,
    pub size: u64,
    pub mode: u32,
}
#[derive(Serialize, Deserialize)]
pub(crate) struct Entry {
    pub name: Vec<u8>,
    pub attributes: Attributes,
}
#[derive(Serialize, Deserialize)]
pub(crate) enum Response {
    Done,
    Attributes(Attributes),
    Entries(Vec<Entry>),
    Handle(u64),
    Data(Vec<u8>),
    Count(usize),
    Error(i32),
}

pub(crate) struct Frame;
impl Frame {
    pub fn decode<T: DeserializeOwned>(bytes: &[u8]) -> io::Result<T> {
        if bytes.len() > MAX_FRAME {
            return Err(io::Error::new(
                io::ErrorKind::InvalidData,
                "oversized FSKit packet",
            ));
        }
        let (value, used) = bincode::serde::decode_from_slice(
            &bytes,
            bincode::config::standard().with_limit::<MAX_FRAME>(),
        )
        .map_err(io::Error::other)?;
        if used != bytes.len() {
            return Err(io::Error::new(
                io::ErrorKind::InvalidData,
                "trailing FSKit frame data",
            ));
        }
        Ok(value)
    }
    pub fn encode(value: &impl Serialize) -> io::Result<Vec<u8>> {
        let bytes = bincode::serde::encode_to_vec(value, bincode::config::standard())
            .map_err(io::Error::other)?;
        if bytes.len() > MAX_FRAME {
            return Err(io::Error::new(
                io::ErrorKind::InvalidData,
                "oversized FSKit frame",
            ));
        }
        Ok(bytes)
    }
}
