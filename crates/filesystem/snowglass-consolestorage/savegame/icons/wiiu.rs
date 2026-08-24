use std::io::BufReader;

use image::{DynamicImage, codecs::tga::TgaDecoder};

use super::{IconSave, SaveIcon};
use crate::impls::wiiu::nand::Save;

impl IconSave for Save<'_> {
    fn read_icon(&self) -> Option<SaveIcon> {
        let source = self
            .storage
            .open_read(&format!("{}/meta/iconTex.tga", self.root))
            .ok()?;
        TgaDecoder::new(BufReader::new(source))
            .ok()
            .and_then(|decoder| DynamicImage::from_decoder(decoder).ok())
            .map(|image| SaveIcon::Image(image.to_rgba8()))
    }
}
