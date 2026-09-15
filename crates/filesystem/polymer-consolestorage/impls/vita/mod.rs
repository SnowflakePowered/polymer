//! Vita storage uses native device directories beneath one volume root.
//! Save volumes use one fixed guest user, independently of Polymer profiles.
mod depots;
mod nand;
mod save_repo;

pub use depots::VitaDirectoryDepot;
pub use nand::VitaNand;
pub use save_repo::VitaSaveRepo;

pub const GUEST_USER_ROOT: &str = "ux0/user/00";
pub const GUEST_SAVE_ROOT: &str = "ux0/user/00/savedata";
