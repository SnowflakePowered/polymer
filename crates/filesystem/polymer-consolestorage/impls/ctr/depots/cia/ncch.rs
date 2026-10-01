use std::io::{self, Read, Seek, SeekFrom, Write};
use std::ops::Range;

use aes::Aes128;
use aes::cipher::{KeyIvInit, StreamCipher, StreamCipherSeek};
use sha2::{Digest, Sha256};
use zerocopy::byteorder::little_endian::{U16, U32, U64};
use zerocopy::{FromBytes, Immutable, IntoBytes, KnownLayout, Unaligned};

#[derive(FromBytes, IntoBytes, Immutable, KnownLayout, Unaligned)]
#[repr(C)]
pub(in crate::impls::ctr) struct NcchHeader {
    signature: [u8; 0x100],
    pub(super) magic: [u8; 4],
    size: U32,
    partition_id: U64,
    maker_code: [u8; 2],
    version: U16,
    seed_check: U32,
    program_id: U64,
    before_exheader: [u8; 0x60],
    exheader_size: U32,
    reserved: [u8; 4],
    flags: [u8; 8],
    before_exefs: [u8; 0x10],
    exefs_offset: U32,
    exefs_size: U32,
    before_romfs: [u8; 8],
    romfs_offset: U32,
    romfs_size: U32,
    rest: [u8; 0x48],
}

/// A CCI (`.3ds`) image, whose first partition is the executable NCCH.
#[derive(FromBytes, Immutable, KnownLayout, Unaligned)]
#[repr(C)]
pub(in crate::impls::ctr) struct NcsdHeader {
    signature: [u8; 0x100],
    pub(in crate::impls::ctr) magic: [u8; 4],
    size: U32,
    media_id: U64,
    partition_fs_types: [u8; 8],
    partition_crypt_types: [u8; 8],
    partitions: [[U32; 2]; 8],
    rest: [u8; 0xa0],
}

impl NcsdHeader {
    /// Byte offset and length of the executable partition.
    pub(in crate::impls::ctr) fn executable(&self) -> (u64, u64) {
        let [offset, length] = self.partitions[0];
        (
            u64::from(offset.get()) * HEADER_SIZE,
            u64::from(length.get()) * HEADER_SIZE,
        )
    }
}

#[derive(FromBytes, Immutable, KnownLayout, Unaligned)]
#[repr(C)]
pub(super) struct ExeFsEntry {
    pub(super) name: [u8; 8],
    pub(super) offset: U32,
    pub(super) size: U32,
}

const _: () = assert!(size_of::<NcchHeader>() == 0x200);
const _: () = assert!(size_of::<ExeFsEntry>() == 0x10);
const _: () = assert!(size_of::<NcsdHeader>() == 0x200);

const AES_GENERATOR: u128 = 0x1ff9e9aac5fe0408024591dc5d52768a;
const NCCH_KEYX_2C: u128 = 0xb98e95ceca3e4d171f76a94de934c053;
const NCCH_KEYX_25: u128 = 0xcee7d8ab30c00dae850ef5e382ac5af3;
const NCCH_KEYX_18: u128 = 0x82e9c9bebfb8bdb875ecc0a07d474374;
const NCCH_KEYX_1B: u128 = 0x45ad04953992c7c893724a9a7bce6182;
const FIXED_SYSTEM_KEY: u128 = 0x527ce630a9ca305f3696f3cde954194b;
const FIXED_KEY: u8 = 0x01;
const NO_CRYPTO: u8 = 0x04;
const SEED_CRYPTO: u8 = 0x20;
const HEADER_SIZE: u64 = 0x200;
const EXHEADER_SIZE: u64 = 0x800;

#[derive(Clone, Copy)]
enum Region {
    ExHeader = 1,
    ExeFs = 2,
    RomFs = 3,
}

impl NcchHeader {
    fn encrypted(&self) -> bool {
        self.flags[7] & NO_CRYPTO == 0
    }

    fn unit(&self) -> Option<u64> {
        0x200_u64.checked_shl(u32::from(self.flags[6]))
    }

    fn key(&self, key_x: u128, seed: Option<&[u8; 16]>) -> u128 {
        if self.flags[7] & FIXED_KEY != 0 {
            return if (self.program_id.get() >> 32) & 0x10 != 0 {
                FIXED_SYSTEM_KEY
            } else {
                0
            };
        }
        let mut key_y: [u8; 16] = self.signature[..0x10].try_into().expect("fixed signature");
        if let Some(seed) = seed {
            let digest = Sha256::new()
                .chain_update(key_y)
                .chain_update(seed)
                .finalize();
            key_y.copy_from_slice(&digest[..0x10]);
        }
        let key_y = u128::from_be_bytes(key_y);
        ((key_x.rotate_left(2) ^ key_y).wrapping_add(AES_GENERATOR)).rotate_left(87)
    }

    fn secondary_key(&self, seed: Option<&[u8; 16]>) -> io::Result<u128> {
        let program_id = self.program_id.get();
        let seed = if self.flags[7] & (FIXED_KEY | SEED_CRYPTO) == SEED_CRYPTO {
            let seed = seed.ok_or_else(|| {
                io::Error::other(format!(
                    "NCCH seed for {program_id:016X} is not in the key store"
                ))
            })?;
            let check = Sha256::new()
                .chain_update(seed)
                .chain_update(program_id.to_le_bytes())
                .finalize();
            if check[..4] != self.seed_check.get().to_le_bytes() {
                return Err(io::Error::other(format!(
                    "NCCH seed for {program_id:016X} does not match the title"
                )));
            }
            Some(seed)
        } else {
            None
        };
        let key_x = match self.flags[3] {
            0x00 => NCCH_KEYX_2C,
            0x01 => NCCH_KEYX_25,
            0x0a => NCCH_KEYX_18,
            0x0b => NCCH_KEYX_1B,
            method => {
                return Err(io::Error::other(format!(
                    "unknown NCCH encryption method {method:#04x}"
                )));
            }
        };
        Ok(self.key(key_x, seed))
    }

    fn counter(&self, region: Region, offset: u64) -> u128 {
        let partition = self.partition_id.get();
        match self.version.get() {
            1 => (u128::from(partition.swap_bytes()) << 64) | u128::from(offset as u32),
            _ => (u128::from(partition) << 64) | ((region as u128) << 56),
        }
    }

    pub(super) fn decrypt_exefs(&self, exefs_offset: u64, stream_offset: u64, bytes: &mut [u8]) {
        if !self.encrypted() {
            return;
        }
        Keystream {
            range: exefs_offset..u64::MAX,
            origin: exefs_offset,
            key: self.key(NCCH_KEYX_2C, None),
            counter: self.counter(Region::ExeFs, exefs_offset),
        }
        .apply(exefs_offset + stream_offset, bytes);
    }

    /// A decrypted ExeFS file of the NCCH starting at `base`, with `length`
    /// bytes available from there.
    pub(in crate::impls::ctr) fn exefs_file<R: Read + Seek>(
        reader: &mut R,
        base: u64,
        length: u64,
        name: [u8; 8],
    ) -> io::Result<Option<Vec<u8>>> {
        if length < HEADER_SIZE {
            return Ok(None);
        }
        let mut bytes = [0; HEADER_SIZE as usize];
        reader.seek(SeekFrom::Start(base))?;
        reader.read_exact(&mut bytes)?;
        let header = Self::read_from_bytes(&bytes).expect("fixed NCCH header");
        if header.magic != *b"NCCH" {
            return Ok(None);
        }
        let Some((offset, size)) = header.exefs_range(length) else {
            return Ok(None);
        };
        reader.seek(SeekFrom::Start(base + offset))?;
        reader.read_exact(&mut bytes)?;
        header.decrypt_exefs(offset, 0, &mut bytes);
        let Some(entry) = bytes[..0xa0]
            .chunks_exact(size_of::<ExeFsEntry>())
            .map(|bytes| ExeFsEntry::ref_from_bytes(bytes).expect("fixed ExeFS entry"))
            .find(|entry| entry.name == name)
        else {
            return Ok(None);
        };
        let start = HEADER_SIZE + u64::from(entry.offset.get());
        let length = u64::from(entry.size.get());
        if start + length > size {
            return Ok(None);
        }
        let mut file = vec![0; length as usize];
        reader.seek(SeekFrom::Start(base + offset + start))?;
        reader.read_exact(&mut file)?;
        header.decrypt_exefs(offset, start, &mut file);
        Ok(Some(file))
    }

    pub(super) fn exefs_range(&self, length: u64) -> Option<(u64, u64)> {
        let unit = self.unit()?;
        let content_size = u64::from(self.size.get()).checked_mul(unit)?;
        let offset = u64::from(self.exefs_offset.get()).checked_mul(unit)?;
        let size = u64::from(self.exefs_size.get()).checked_mul(unit)?;
        (offset >= 0x200
            && size >= 0x200
            && offset.checked_add(size)? <= content_size
            && content_size <= length)
            .then_some((offset, size))
    }
}

struct Keystream {
    range: Range<u64>,
    origin: u64,
    key: u128,
    counter: u128,
}

impl Keystream {
    fn apply(&self, position: u64, bytes: &mut [u8]) {
        let mut cipher = ctr::Ctr128BE::<Aes128>::new(
            &self.key.to_be_bytes().into(),
            &self.counter.to_be_bytes().into(),
        );
        cipher.seek(position - self.origin);
        cipher.apply_keystream(bytes);
    }
}

struct PendingCode {
    header: Vec<u8>,
    start: u64,
    key: u128,
    counter: u128,
}

/// Removes NCCH encryption from a content stream and marks it `NoCrypto`, as Azahar
/// only loads decrypted titles. Other contents pass through unchanged.
pub(super) struct NcchDecryptor<W> {
    inner: W,
    seed: Option<[u8; 16]>,
    position: u64,
    header: Vec<u8>,
    streams: Vec<Keystream>,
    code: Option<PendingCode>,
}

impl<W: Write> NcchDecryptor<W> {
    pub(super) fn new(inner: W, seed: Option<[u8; 16]>) -> Self {
        Self {
            inner,
            seed,
            position: 0,
            header: Vec::with_capacity(HEADER_SIZE as usize),
            streams: Vec::new(),
            code: None,
        }
    }

    pub(super) fn finish(mut self) -> io::Result<W> {
        if self.position == 0 {
            self.inner.write_all(&self.header)?;
        }
        Ok(self.inner)
    }

    fn begin(&mut self) -> io::Result<()> {
        let header = NcchHeader::mut_from_bytes(&mut self.header).expect("fixed NCCH header");
        if header.magic != *b"NCCH" || !header.encrypted() {
            return Ok(());
        }
        let unit = header
            .unit()
            .ok_or_else(|| io::Error::other("invalid NCCH media unit"))?;
        let primary = header.key(NCCH_KEYX_2C, None);
        let secondary = header.secondary_key(self.seed.as_ref())?;
        if header.exheader_size.get() != 0 {
            self.streams.push(Keystream {
                range: HEADER_SIZE..HEADER_SIZE + EXHEADER_SIZE,
                origin: HEADER_SIZE,
                key: primary,
                counter: header.counter(Region::ExHeader, HEADER_SIZE),
            });
        }
        let exefs = u64::from(header.exefs_offset.get()) * unit;
        if header.exefs_size.get() != 0 {
            let counter = header.counter(Region::ExeFs, exefs);
            self.streams.push(Keystream {
                range: exefs..exefs + u64::from(header.exefs_size.get()) * unit,
                origin: exefs,
                key: primary,
                counter,
            });
            self.code = Some(PendingCode {
                header: Vec::with_capacity(HEADER_SIZE as usize),
                start: exefs,
                key: secondary,
                counter,
            });
        }
        let romfs = u64::from(header.romfs_offset.get()) * unit;
        if header.romfs_size.get() != 0 {
            self.streams.push(Keystream {
                range: romfs..romfs + u64::from(header.romfs_size.get()) * unit,
                origin: romfs,
                key: secondary,
                counter: header.counter(Region::RomFs, romfs),
            });
        }
        header.flags[3] = 0;
        header.flags[7] = (header.flags[7] & !(FIXED_KEY | SEED_CRYPTO)) | NO_CRYPTO;
        Ok(())
    }

    fn decrypt(&mut self, bytes: &mut [u8]) {
        let mut offset = 0;
        while offset < bytes.len() {
            let position = self.position + offset as u64;
            let end = position + (bytes.len() - offset) as u64;
            let boundary = self
                .streams
                .iter()
                .flat_map(|stream| [stream.range.start, stream.range.end])
                .chain(self.code.as_ref().map(|code| code.start + HEADER_SIZE))
                .filter(|boundary| *boundary > position)
                .fold(end, u64::min);
            let segment = &mut bytes[offset..offset + (boundary - position) as usize];
            if let Some(stream) = self
                .streams
                .iter()
                .rev()
                .find(|stream| stream.range.contains(&position))
            {
                stream.apply(position, segment);
            }
            self.capture_code(position, segment);
            offset += segment.len();
        }
    }

    fn capture_code(&mut self, position: u64, bytes: &[u8]) {
        let Some(code) = self.code.as_mut() else {
            return;
        };
        if !(code.start..code.start + HEADER_SIZE).contains(&position) {
            return;
        }
        code.header.extend_from_slice(bytes);
        if code.header.len() < HEADER_SIZE as usize {
            return;
        }
        let code = self.code.take().expect("pending ExeFS header");
        let files =
            <[ExeFsEntry]>::ref_from_bytes(&code.header[..0xa0]).expect("fixed ExeFS entries");
        self.streams.extend(
            files
                .iter()
                .filter(|file| file.name == *b".code\0\0\0")
                .map(|file| {
                    let start = code.start + HEADER_SIZE + u64::from(file.offset.get());
                    Keystream {
                        range: start..start + u64::from(file.size.get()),
                        origin: code.start,
                        key: code.key,
                        counter: code.counter,
                    }
                }),
        );
    }
}

impl<W: Write> Write for NcchDecryptor<W> {
    fn write(&mut self, bytes: &[u8]) -> io::Result<usize> {
        let mut rest = bytes;
        if self.position == 0 {
            let take = rest.len().min(HEADER_SIZE as usize - self.header.len());
            self.header.extend_from_slice(&rest[..take]);
            rest = &rest[take..];
            if self.header.len() < HEADER_SIZE as usize {
                return Ok(bytes.len());
            }
            self.begin()?;
            self.inner.write_all(&self.header)?;
            self.position = HEADER_SIZE;
        }
        let mut buffer = rest.to_vec();
        self.decrypt(&mut buffer);
        self.inner.write_all(&buffer)?;
        self.position += buffer.len() as u64;
        Ok(bytes.len())
    }

    fn flush(&mut self) -> io::Result<()> {
        self.inner.flush()
    }
}
