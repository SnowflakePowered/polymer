mod crypto;
mod depot;
mod ticket;
mod tmd;

pub use depot::WiiWadDepot;

use crate::Error;

fn signed_body<'a>(bytes: &'a [u8], file: &str) -> Result<(usize, &'a [u8]), Error> {
    let signature_type = u32::from_be_bytes(
        *bytes
            .first_chunk()
            .ok_or_else(|| Error::malformed(file, "missing signature type"))?,
    );
    let size = match signature_type {
        0x0001_0000 => 0x240,
        0x0001_0001 => 0x140,
        0x0001_0002 => 0x80,
        _ => return Err(Error::malformed(file, "unsupported signature type")),
    };
    let body = bytes
        .get(size..)
        .ok_or_else(|| Error::malformed(file, "truncated signed container"))?;
    Ok((size, body))
}
