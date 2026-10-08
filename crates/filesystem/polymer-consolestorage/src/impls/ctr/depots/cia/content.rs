use std::fs::File;
use std::io::{self, Read, Seek, SeekFrom};
use std::path::Path;

use aes::Aes128;
use aes::cipher::{BlockDecryptMut, KeyIvInit};
use csis::CiaArchive;

use crate::impls::ctr::ticket::CtrTicket;

/// Random access to the first CIA content, removing its title-key CBC layer
/// without streaming the whole content.
pub(in crate::impls::ctr) struct CiaContent {
    file: File,
    start: u64,
    length: u64,
    index: u16,
    key: Option<[u8; 16]>,
    position: u64,
}

impl CiaContent {
    pub(in crate::impls::ctr) fn open(path: &Path, archive: &CiaArchive<File>) -> Option<Self> {
        let header = archive.header();
        let record = archive.items().first()?;
        let tmd_end = [
            header.certificates_size,
            header.ticket_size,
            header.tmd_size,
        ]
        .into_iter()
        .fold(u64::from(header.header_size), |cursor, size| {
            ((cursor + 63) & !63) + u64::from(size)
        });
        let key = if record.content_type.is_encrypted() {
            Some(CtrTicket::from_cia(path).ok()?.bytes.try_into().ok()?)
        } else {
            None
        };
        Some(Self {
            file: File::open(path).ok()?,
            start: (tmd_end + 63) & !63,
            length: record.size,
            index: record.content_index,
            key,
            position: 0,
        })
    }

    pub(in crate::impls::ctr) fn length(&self) -> u64 {
        self.length
    }
}

impl Read for CiaContent {
    fn read(&mut self, buf: &mut [u8]) -> io::Result<usize> {
        let count = (buf.len() as u64)
            .min(self.length.saturating_sub(self.position))
            .min(0x10000) as usize;
        if count == 0 {
            return Ok(0);
        }
        let Some(key) = self.key else {
            self.file
                .seek(SeekFrom::Start(self.start + self.position))?;
            let read = self.file.read(&mut buf[..count])?;
            self.position += read as u64;
            return Ok(read);
        };
        let first = self.position & !15;
        let end = (self.position + count as u64 + 15) & !15;
        let mut iv = [0; 16];
        if first == 0 {
            iv[..2].copy_from_slice(&self.index.to_be_bytes());
        } else {
            self.file.seek(SeekFrom::Start(self.start + first - 16))?;
            self.file.read_exact(&mut iv)?;
        }
        let mut blocks = vec![0; (end - first) as usize];
        self.file.seek(SeekFrom::Start(self.start + first))?;
        self.file.read_exact(&mut blocks)?;
        let mut cipher = cbc::Decryptor::<Aes128>::new(&key.into(), &iv.into());
        for block in blocks.chunks_exact_mut(16) {
            cipher.decrypt_block_mut(block.into());
        }
        let offset = (self.position - first) as usize;
        buf[..count].copy_from_slice(&blocks[offset..offset + count]);
        self.position += count as u64;
        Ok(count)
    }
}

impl Seek for CiaContent {
    fn seek(&mut self, pos: SeekFrom) -> io::Result<u64> {
        self.position = match pos {
            SeekFrom::Start(offset) => Some(offset),
            SeekFrom::End(offset) => self.length.checked_add_signed(offset),
            SeekFrom::Current(offset) => self.position.checked_add_signed(offset),
        }
        .ok_or_else(|| io::Error::other("invalid CIA content seek"))?;
        Ok(self.position)
    }
}
