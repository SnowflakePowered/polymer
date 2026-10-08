use super::TitleId;
use crate::{
    Error,
    keys::{KeyIdentity, KeyRecord},
};
use std::{fs::File, io::Read, path::Path};

pub struct Rap;

impl Rap {
    pub fn title_id(content_id: &str) -> Option<&str> {
        let id = content_id.as_bytes();
        (id.len() == 36
            && id
                .iter()
                .all(|byte| byte.is_ascii_alphanumeric() || matches!(byte, b'-' | b'_'))
            && id[6] == b'-'
            && id[16] == b'_'
            && id[17..19].iter().all(u8::is_ascii_digit)
            && id[19] == b'-'
            && TitleId::parse(&content_id[7..16]).is_some())
        .then(|| &content_id[7..16])
    }

    pub fn read(path: &Path) -> Result<KeyRecord, Error> {
        let content_id = path
            .file_stem()
            .and_then(|stem| stem.to_str())
            .ok_or_else(|| Error::InvalidPath(path.display().to_string()))?;
        let Some(title_id) = Self::title_id(content_id) else {
            return Err(Error::malformed(
                path.display().to_string(),
                "RAP filename must contain a PS3 content ID",
            ));
        };
        let mut bytes = Vec::new();
        File::open(path)
            .and_then(|file| file.take(17).read_to_end(&mut bytes))
            .map_err(|e| Error::io(path, e))?;
        if bytes.len() != 16 {
            return Err(Error::malformed(
                path.display().to_string(),
                "a RAP must be exactly 16 bytes",
            ));
        }
        Ok(KeyRecord {
            identity: KeyIdentity {
                namespace: "content".into(),
                id: content_id.to_owned(),
            },
            title_id: Some(title_id.to_owned()),
            bytes,
        })
    }
}
