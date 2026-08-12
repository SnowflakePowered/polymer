use crate::installable::Artifact;
use crate::savegame::{SaveGame, TreeSaveGame};
use crate::Storage;

pub enum NandCompletion {
    /// The NAND is complete as far as minimally
    Minimal,
    Full,
    Verified
}

pub trait VirtualNand<S: TreeSaveGame, A: Artifact> {
    fn savegames(&self) -> impl Iterator<Item = S>;

    fn installed(&self) -> impl Iterator<Item = A>;

    fn root(&self) -> &str;

    fn install_artifact(&mut self, installable: A::Package);

    /// Mount a savegame to the nand
    fn mount_savegame(&mut self, savegame: S);

    fn new_empty<St: Storage>(storage: &St) -> Self;
    fn is_complete(&self, completion_criteria: NandCompletion) -> bool;

    /// Clone the base, ignoring save games.
    fn clone_base<St: Storage>(&self, storage: &St) -> Self;
}


