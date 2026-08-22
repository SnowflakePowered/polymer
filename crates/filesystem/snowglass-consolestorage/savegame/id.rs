/// A unique ID for a save.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct SaveDepotId(pub u64);

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct SonyProductCode<'a>(&'a [u8; 12]);

impl<'a> SonyProductCode<'a> {
    pub fn new(code: &'a [u8]) -> Option<Self> {
        let code = <&[u8; 12]>::try_from(code).ok()?;
        (code[6] == b'-'
            && code[2..6].iter().all(u8::is_ascii_alphabetic)
            && code[7..].iter().all(u8::is_ascii_digit))
        .then_some(Self(code))
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct NintendoTitleId(u64);

impl NintendoTitleId {
    pub fn new_from_parts(high: u32, low: u32) -> Self {
        Self((u64::from(high) << 32) | u64::from(low))
    }

    pub fn new(id: u64) -> Self {
        Self(id)
    }
}

impl From<SonyProductCode<'_>> for SaveDepotId {
    fn from(code: SonyProductCode<'_>) -> SaveDepotId {
        let product_code = code.0;

        let mut number = 0u32;
        for digit in &product_code[7..] {
            number = number * 10 + u32::from(digit - b'0');
        }

        let nibble = |byte: u8| match byte.to_ascii_uppercase() {
            byte @ b'0'..=b'9' => byte - b'0',
            byte @ b'A'..=b'Z' => (byte - b'A' + 10) & 0x0f,
            byte => byte & 0x0f,
        };
        let mut encoded = [0; 8];
        encoded[0] = nibble(product_code[0]) << 4 | nibble(product_code[1]);
        encoded[1..5].copy_from_slice(&product_code[2..6]);
        encoded[5..].copy_from_slice(&number.to_le_bytes()[..3]);
        SaveDepotId(u64::from_le_bytes(encoded))
    }
}

impl From<NintendoTitleId> for SaveDepotId {
    fn from(title_id: NintendoTitleId) -> Self {
        Self(title_id.0)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn product_codes_have_reversible_depot_ids() {
        assert_eq!(
            SaveDepotId::from(SonyProductCode::new(b"BASLUS-00707").unwrap())
                .0
                .to_le_bytes(),
            [0xba, b'S', b'L', b'U', b'S', 0xc3, 0x02, 0x00]
        );
        assert_eq!(
            SaveDepotId::from(SonyProductCode::new(b"BISLPS-97512").unwrap())
                .0
                .to_le_bytes(),
            [0xb2, b'S', b'L', b'P', b'S', 0xe8, 0x7c, 0x01]
        );
        assert_eq!(
            SaveDepotId::from(SonyProductCode::new(b"BKSLKA-00001").unwrap())
                .0
                .to_le_bytes()[0],
            0xb4
        );
        assert_eq!(
            SaveDepotId::from(SonyProductCode::new(b"BZSLUS-00001").unwrap())
                .0
                .to_le_bytes()[0],
            0xb3
        );
    }

    #[test]
    fn nintendo_title_ids_preserve_the_native_id() {
        let depot: SaveDepotId = NintendoTitleId::new(0x0005_0000_101c_9500).into();
        assert_eq!(depot.0, 0x0005_0000_101c_9500);
    }
}
