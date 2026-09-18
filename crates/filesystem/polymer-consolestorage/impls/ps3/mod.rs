pub mod depots;
mod nand;
pub mod rap;
mod save_repo;

pub use nand::Ps3Nand;
pub use save_repo::Ps3SaveRepo;

pub(crate) use super::sony::TitleId;

const GUEST_SAVE_ROOT: &str = "dev_hdd0/home/00000001/savedata";
const GUEST_USER_ROOT: &str = "dev_hdd0/home/00000001";
