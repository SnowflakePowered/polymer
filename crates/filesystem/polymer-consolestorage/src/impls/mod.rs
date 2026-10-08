pub mod battery;
pub mod ctr;
pub mod dreamcast;
mod es;
pub mod gamecube;
pub mod memorycard;
pub mod ps1;
pub mod ps2;
pub mod ps3;
pub mod psp;
pub(crate) mod sony;
pub mod vita;
pub mod wii;
pub mod wiiu;

impl dyn crate::nand::composition::NandComposer {
    pub(crate) fn for_platform(
        platform: &str,
    ) -> Option<&'static dyn crate::nand::composition::NandComposer> {
        match platform {
            vita::VitaNand::PLATFORM => Some(&vita::composer::VitaComposer),
            ps3::Ps3Nand::PLATFORM => Some(&ps3::composer::Ps3Composer),
            wii::WiiNand::PLATFORM => Some(&wii::composer::WiiComposer),
            ctr::CtrNand::PLATFORM | ctr::CtrNand::NEW_PLATFORM => {
                Some(&ctr::composer::CtrComposer)
            }
            wiiu::nand::WiiUNand::PLATFORM => Some(&wiiu::composer::WiiUComposer),
            _ => None,
        }
    }
}

pub use crate::savegame::id::{
    DreamcastProductId, GameCubeDiscId, NintendoTitleId, SonyProductCode,
};
