pub mod dreamcast;
pub mod gamecube;
pub mod memorycard;
pub mod ps1;
pub mod ps2;
pub mod wiiu;

pub use crate::savegame::id::{
    DreamcastProductId, GameCubeDiscId, NintendoTitleId, SonyProductCode,
};
