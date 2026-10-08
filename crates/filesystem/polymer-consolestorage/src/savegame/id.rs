/// A unique ID for a save.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Ord, PartialOrd)]
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

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct GameCubeDiscId([u8; 6]);

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, PartialOrd, Ord)]
pub struct DreamcastProductId {
    bytes: [u8; 10],
    len: u8,
}

impl DreamcastProductId {
    pub fn new(code: &[u8]) -> Option<Self> {
        if code.is_empty() || code.len() > 10 {
            return None;
        }
        let mut bytes = [0; 10];
        for (destination, source) in bytes.iter_mut().zip(code.iter().copied()) {
            if !source.is_ascii_alphanumeric() && !matches!(source, b'-' | b' ') {
                return None;
            }
            *destination = source;
        }
        Some(Self {
            len: u8::try_from(code.len()).unwrap(),
            bytes,
        })
    }

    pub fn as_bytes(&self) -> &[u8] {
        &self.bytes[..usize::from(self.len)]
    }
}

impl std::fmt::Display for DreamcastProductId {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter.write_str(std::str::from_utf8(self.as_bytes()).expect("validated ASCII"))
    }
}

impl GameCubeDiscId {
    pub fn new(code: &[u8]) -> Option<Self> {
        let code: [u8; 6] = code.try_into().ok()?;
        code.iter()
            .all(u8::is_ascii_alphanumeric)
            .then_some(Self(code))
    }

    pub fn as_bytes(&self) -> &[u8; 6] {
        &self.0
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

impl From<GameCubeDiscId> for SaveDepotId {
    fn from(game_code: GameCubeDiscId) -> Self {
        let mut encoded = [0; 8];
        encoded[..6].copy_from_slice(&game_code.0);
        encoded[6..].copy_from_slice(b"GC");
        Self(u64::from_le_bytes(encoded))
    }
}

impl From<DreamcastProductId> for SaveDepotId {
    fn from(product: DreamcastProductId) -> Self {
        let digit = |byte: u8| match byte {
            0 => 0,
            b'0'..=b'9' => byte - b'0' + 1,
            b'A'..=b'Z' => byte - b'A' + 11,
            b'a'..=b'z' => byte - b'a' + 37,
            b'-' => 63,
            b' ' => 64,
            _ => unreachable!("Dreamcast product IDs are validated"),
        };
        let encoded = product
            .bytes
            .into_iter()
            .fold(0u64, |encoded, byte| encoded * 65 + u64::from(digit(byte)));
        Self(encoded)
    }
}

impl TryFrom<SaveDepotId> for DreamcastProductId {
    type Error = ();

    fn try_from(id: SaveDepotId) -> Result<Self, Self::Error> {
        let mut encoded = id.0;
        let mut bytes = [0; 10];
        for byte in bytes.iter_mut().rev() {
            let digit = (encoded % 65) as u8;
            encoded /= 65;
            *byte = match digit {
                0 => 0,
                1..=10 => b'0' + digit - 1,
                11..=36 => b'A' + digit - 11,
                37..=62 => b'a' + digit - 37,
                63 => b'-',
                64 => b' ',
                _ => unreachable!(),
            };
        }
        if encoded != 0 {
            return Err(());
        }
        let len = bytes.iter().position(|byte| *byte == 0).unwrap_or(10);
        if bytes[len..].iter().any(|byte| *byte != 0) {
            return Err(());
        }
        DreamcastProductId::new(&bytes[..len]).ok_or(())
    }
}

impl TryFrom<SaveDepotId> for GameCubeDiscId {
    type Error = ();

    fn try_from(id: SaveDepotId) -> Result<Self, Self::Error> {
        let encoded = id.0.to_le_bytes();
        if &encoded[6..] != b"GC" {
            return Err(());
        }
        Self::new(&encoded[..6]).ok_or(())
    }
}
