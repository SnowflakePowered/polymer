use aes::Aes128;
use aes::cipher::{BlockDecrypt, KeyInit, generic_array::GenericArray};
use sha1::{Digest, Sha1};

use super::depot::WiiWadDepot;
use super::tmd::Content;
use crate::Error;

const COMMON_KEYS: [[u8; 16]; 2] = [
    [
        0xeb, 0xe4, 0x2a, 0x22, 0x5e, 0x85, 0x93, 0xe4, 0x48, 0xd9, 0xc5, 0x45, 0x73, 0x81, 0xaa,
        0xf7,
    ],
    [
        0x63, 0xb8, 0x2b, 0xb4, 0xf4, 0x61, 0x4e, 0x2e, 0x13, 0xf2, 0xfe, 0xfb, 0xba, 0x4c, 0x9b,
        0x7e,
    ],
];

pub(super) struct SelectedTitleKey {
    pub(super) index: u8,
    pub(super) key: [u8; 16],
}

impl WiiWadDepot {
    pub(super) fn select_title_key(&self) -> Result<SelectedTitleKey, Error> {
        let specified = usize::from(self.ticket.common_key_index);
        let mut candidates = Vec::new();
        if specified < COMMON_KEYS.len() {
            candidates.push(specified);
        } else if !self.signature_is_zero() {
            return Err(Error::Unsupported(format!(
                "Wii common key index {} is not supported",
                self.ticket.common_key_index
            )));
        }
        if self.signature_is_zero() {
            candidates.extend((0..COMMON_KEYS.len()).filter(|index| *index != specified));
        }
        let probe = self
            .tmd
            .contents
            .iter()
            .min_by_key(|content| content.size)
            .ok_or_else(|| Error::malformed("TMD", "title has no contents"))?;
        let mut specified_key = None;
        for index in candidates {
            let key = self.decrypt_title_key(&COMMON_KEYS[index])?;
            if index == specified {
                specified_key = Some(key);
            }
            if self.decrypt_content(probe, &key).is_ok() {
                return Ok(SelectedTitleKey {
                    index: index as u8,
                    key,
                });
            }
        }
        if let Some(key) = specified_key {
            return Ok(SelectedTitleKey {
                index: specified as u8,
                key,
            });
        }
        Err(Error::IntegrityMatchFailed {
            failed: "WAD title key".to_owned(),
        })
    }

    fn decrypt_title_key(&self, common_key: &[u8; 16]) -> Result<[u8; 16], Error> {
        let mut iv = [0; 16];
        iv[..8].copy_from_slice(&self.ticket.title_id.to_be_bytes());
        let decrypted = decrypt_cbc(common_key, &iv, &self.ticket.encrypted_title_key)?;
        let mut title_key = [0; 16];
        title_key.copy_from_slice(&decrypted);
        Ok(title_key)
    }

    pub(super) fn decrypt_content(
        &self,
        content: &Content,
        title_key: &[u8; 16],
    ) -> Result<Vec<u8>, Error> {
        let encrypted = self.read_encrypted(content)?;
        let mut iv = [0; 16];
        iv[..2].copy_from_slice(&content.index.to_be_bytes());
        let mut decrypted = decrypt_cbc(title_key, &iv, &encrypted)?;
        decrypted.truncate(
            usize::try_from(content.size)
                .map_err(|_| Error::malformed("TMD", "content is too large for this host"))?,
        );
        if Sha1::digest(&decrypted).as_slice() != content.sha1 {
            return Err(Error::IntegrityMatchFailed {
                failed: format!("{:08x}.app", content.id),
            });
        }
        Ok(decrypted)
    }
}

fn decrypt_cbc(key: &[u8; 16], iv: &[u8; 16], bytes: &[u8]) -> Result<Vec<u8>, Error> {
    if !bytes.len().is_multiple_of(16) {
        return Err(Error::malformed("WAD", "AES data is not block aligned"));
    }
    let cipher = Aes128::new(GenericArray::from_slice(key));
    let mut previous = *iv;
    let mut output = Vec::with_capacity(bytes.len());
    for encrypted in bytes.chunks_exact(16) {
        let mut block = GenericArray::clone_from_slice(encrypted);
        cipher.decrypt_block(&mut block);
        for (plain, chain) in block.iter_mut().zip(previous) {
            *plain ^= chain;
        }
        output.extend_from_slice(&block);
        previous.copy_from_slice(encrypted);
    }
    Ok(output)
}
