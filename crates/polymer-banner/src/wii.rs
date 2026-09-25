//! Wii Menu channel banners: the animated `icon.bin` and `banner.bin` layouts
//! inside a channel's opening banner.
use bannerbobomb_parser::brfnt::Brfnt;
use bannerbobomb_parser::brlan::Brlan;
use bannerbobomb_parser::brlyt::{Brlyt, Pane};
use bannerbobomb_parser::timeline::Timeline;
use bannerbobomb_parser::tpl::{Tpl, TplImage};
use std::collections::{HashMap, HashSet};
use u8arc::{Entry, FileEntry, U8Arc};

/// A channel layout with its animations and decoded textures.
pub struct WiiBanner {
    pub layout: Brlyt,
    pub timeline: Timeline,
    /// The first image of each TPL, keyed by lowercased file name.
    pub textures: HashMap<String, TplImage>,
    /// Text pane fonts from the archive or the System Menu, keyed by lowercased name.
    pub fonts: HashMap<String, Brfnt>,
    /// Localized panes hidden because they belong to another locale.
    pub hidden: HashSet<String>,
    /// The region the Wii Menu shows, as `[x, y, width, height]` in layout
    /// units with y pointing down.
    pub viewport: [f32; 4],
}

impl WiiBanner {
    /// A decompressed `icon.bin` or `banner.bin` U8 archive.
    pub const MEDIA_TYPE: &str = "application/x-wii-banner";
    const U8_MAGIC: [u8; 4] = [0x55, 0xaa, 0x38, 0x2d];
    const MAX_ARCHIVE_SIZE: usize = 16 * 1024 * 1024;
    const ICON_SIZE: [f32; 2] = [128.0, 96.0];
    const BANNER_SIZE: [f32; 2] = [608.0, 342.0];
    const LANGUAGE: &str = "ENG";

    /// Parse a decompressed channel layout archive, as stored under [`Self::MEDIA_TYPE`].
    pub fn parse(archive: &[u8]) -> Option<Self> {
        let archive = U8Arc::read(archive).ok()?;
        let layouts = Self::directory(&archive, "blyt");
        let (name, layout) = ["icon", "banner"]
            .into_iter()
            .find_map(|name| Some((name, layouts.get(&format!("{name}.brlyt"))?)))?;
        let layout = Brlyt::parse(layout).ok()?;
        let animations = Self::animations(&Self::directory(&archive, "anim"), name);
        let timeline = Timeline::new(&animations);
        let textures = Self::directory(&archive, "timg")
            .into_iter()
            .filter_map(|(name, bytes)| {
                Some((name, Tpl::parse(bytes).ok()?.images.into_iter().next()?))
            })
            .collect();
        let archived = Self::directory(&archive, "font");
        let fonts = layout
            .fonts
            .iter()
            .filter_map(|name| {
                let name = name.to_ascii_lowercase();
                let font = match archived.get(&name) {
                    Some(bytes) => Brfnt::parse(bytes).ok(),
                    None => Brfnt::builtin(&name)?.ok(),
                };
                Some((name, font?))
            })
            .collect();
        let (localized, active) = layout.language_panes(Some(Self::LANGUAGE));
        let hidden = localized.difference(&active).cloned().collect();
        let viewport = Self::viewport(&layout, name == "icon");
        Some(Self {
            layout,
            timeline,
            textures,
            fonts,
            hidden,
            viewport,
        })
    }

    /// The decompressed `icon.bin` archive of a channel's opening banner
    /// (content 0), ready to store under [`Self::MEDIA_TYPE`].
    pub fn icon_archive(opening: &[u8]) -> Option<Vec<u8>> {
        let archive = opening
            .windows(Self::U8_MAGIC.len())
            .enumerate()
            .filter(|(_, window)| *window == Self::U8_MAGIC)
            .find_map(|(offset, _)| U8Arc::read(&opening[offset..]).ok())?;
        let icon = archive
            .get_entry_data("meta/icon.bin")
            .or_else(|| archive.get_entry_data("icon.bin"))?;
        let archive = Self::imd5_payload(icon)?;
        let archive = match archive.strip_prefix(b"LZ77") {
            Some(compressed) => Self::decompress_lz10(compressed)?,
            None => archive.to_vec(),
        };
        U8Arc::read(&archive).ok()?;
        Some(archive)
    }

    /// Whether a pane is drawn in the chosen locale.
    pub fn shows(&self, pane: &Pane) -> bool {
        !self.hidden.contains(&pane.name)
    }

    /// The Wii Menu plays `{layout}_Start` then `{layout}_Loop`, or a single `{layout}` timeline.
    fn animations(files: &HashMap<String, &[u8]>, layout: &str) -> Vec<Brlan> {
        let sequenced = [
            format!("{layout}_start.brlan"),
            format!("{layout}_loop.brlan"),
        ];
        let names = if sequenced.iter().any(|name| files.contains_key(name)) {
            sequenced.to_vec()
        } else {
            vec![format!("{layout}.brlan")]
        };
        names
            .iter()
            .filter_map(|name| Brlan::parse(files.get(name)?).ok())
            .collect()
    }

    /// A centered 4:3 icon aperture, or the top of a 16:9 banner.
    fn viewport(layout: &Brlyt, icon: bool) -> [f32; 4] {
        let positive = |value: f32, fallback: f32| {
            if value.is_finite() && value > 0.0 {
                value
            } else {
                fallback
            }
        };
        let width = positive(layout.layout.width, 608.0);
        let height = positive(layout.layout.height, 456.0);
        let [view_width, view_height] = if icon {
            Self::ICON_SIZE
        } else {
            [
                Self::BANNER_SIZE[0].min(width),
                Self::BANNER_SIZE[1].min(height),
            ]
        };
        let origin = match (layout.layout.centered, icon) {
            (true, true) => [-view_width / 2.0, -view_height / 2.0],
            (true, false) => [-view_width / 2.0, -height / 2.0],
            (false, _) => [0.0, 0.0],
        };
        [origin[0], origin[1], view_width, view_height]
    }

    /// Files of `arc/{directory}`, keyed by lowercased name.
    fn directory<'a>(archive: &'a U8Arc<'_>, directory: &str) -> HashMap<String, &'a [u8]> {
        let named = |files: &'a [Entry], name: &str| {
            files.iter().find_map(|entry| match entry {
                Entry::DirEntry { name: found, files } if found.eq_ignore_ascii_case(name) => {
                    Some(files.as_slice())
                }
                _ => None,
            })
        };
        let root = archive.get_root_entry();
        let Some(files) = named(root, "arc")
            .or_else(|| named(root, ".").and_then(|files| named(files, "arc")))
            .and_then(|arc| named(arc, directory))
        else {
            return HashMap::new();
        };
        files
            .iter()
            .filter_map(|entry| match entry {
                Entry::FileEntry {
                    name,
                    data: FileEntry::Data(bytes),
                } => Some((name.to_ascii_lowercase(), bytes.as_slice())),
                Entry::FileEntry {
                    name,
                    data: FileEntry::Ref { offset, length },
                } => {
                    let start = usize::try_from(*offset).ok()?;
                    let end = start.checked_add(usize::try_from(*length).ok()?)?;
                    Some((
                        name.to_ascii_lowercase(),
                        archive.get_data().get(start..end)?,
                    ))
                }
                Entry::DirEntry { .. } => None,
            })
            .collect()
    }

    fn imd5_payload(bytes: &[u8]) -> Option<&[u8]> {
        if bytes.get(..4) != Some(b"IMD5") {
            return None;
        }
        let size = u32::from_be_bytes(bytes.get(4..8)?.try_into().ok()?);
        bytes.get(0x20..0x20_usize.checked_add(usize::try_from(size).ok()?)?)
    }

    /// Nintendo LZ10: a type byte, a little-endian 24-bit size, then flag-led
    /// runs of literals and 12-bit back-references.
    fn decompress_lz10(bytes: &[u8]) -> Option<Vec<u8>> {
        if bytes.first() != Some(&0x10) {
            return None;
        }
        let size = u32::from_le_bytes([*bytes.get(1)?, *bytes.get(2)?, *bytes.get(3)?, 0]);
        let size = usize::try_from(size)
            .ok()
            .filter(|size| *size <= Self::MAX_ARCHIVE_SIZE)?;
        let mut input = bytes.get(4..)?.iter().copied();
        let mut output = Vec::with_capacity(size);
        while output.len() < size {
            let flags = input.next()?;
            for bit in (0..8).rev() {
                if output.len() == size {
                    break;
                }
                if flags & (1 << bit) == 0 {
                    output.push(input.next()?);
                    continue;
                }
                let pair = [input.next()?, input.next()?];
                let length = usize::from(pair[0] >> 4) + 3;
                let distance = usize::from(u16::from_be_bytes(pair) & 0x0fff) + 1;
                if distance > output.len() || output.len() + length > size {
                    return None;
                }
                for _ in 0..length {
                    output.push(output[output.len() - distance]);
                }
            }
        }
        Some(output)
    }
}
