use polymer_banner::ps2::{IconSys, Ps2Icon};

use super::{IconSave, SaveIcon};
use crate::impls::ps2::IconSaveContainer;

impl IconSaveContainer<'_> {
    fn file(&self, name: &[u8]) -> Option<&[u8]> {
        self.files()
            .find(|file| file.name_bytes().eq_ignore_ascii_case(name))
            .map(|file| file.data())
    }

    fn icon_sys(&self) -> Option<&IconSys> {
        IconSys::parse(self.file(IconSys::FILE_NAME)?)
    }

    pub(crate) fn title(&self) -> Option<String> {
        let title = self
            .icon_sys()?
            .title_lines()
            .iter()
            .map(|line| encoding_rs::SHIFT_JIS.decode_without_bom_handling(line).0)
            .map(|line| line.trim().to_owned())
            .filter(|line| !line.is_empty())
            .collect::<Vec<_>>()
            .join(" ");
        (!title.is_empty()).then_some(title)
    }
}

impl IconSave for IconSaveContainer<'_> {
    fn read_icon(&self) -> Option<SaveIcon> {
        let icon_sys = self.file(IconSys::FILE_NAME)?;
        let icon = self.file(IconSys::parse(icon_sys)?.normal_icon())?;
        let bytes = [icon_sys, icon].concat();
        Ps2Icon::parse(&bytes)?;
        Some(SaveIcon::Ps2Icon(bytes))
    }
}
