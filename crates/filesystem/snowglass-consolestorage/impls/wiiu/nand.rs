use crate::nand::{NandCompletion, Permissions, VirtualNand, permissions_for};
use crate::{Error, Storage, ensure_structure, requires_structure};

const COUNTRY_FILE: &str = include_str!("./static/country.txt");
const LANGUAGE_FILE: &str = include_str!("./static/language.txt");

/// Installed titles and system content are read-only to the guest; saves and
/// anything Cemu scribbles are not. The registry is ours, not the guest's.
///
/// The root stays writable because Cemu write-tests it on every launch, by
/// creating and deleting `writetestdummy` in `CreateDefaultMLCFiles`, and exits
/// if that fails.
const LAYOUT: &[(&str, Permissions)] = &[
  ("", Permissions::all()),
  ("sys", Permissions::READ_EXECUTE),
  ("usr/title", Permissions::READ_EXECUTE),
  ("usr/save", Permissions::all()),
  (".manifest.db", Permissions::empty()),
];

pub struct WiiUNand {
  storage: Box<dyn Storage>,
}

impl VirtualNand for WiiUNand {
  fn root(&self) -> &dyn Storage {
    &*self.storage
  }

  fn platform(&self) -> &str {
    "NINTENDO_WIIU"
  }

  async fn new_empty<St: Storage + 'static>(
    storage: St,
    label: Option<String>,
  ) -> Result<Self, Error> {
    let storage: Box<dyn Storage> = Box::new(storage);

    ensure_structure!(&*storage,
      "sys/",
        "sys/title/0005001b/1005c000/content/country.txt": COUNTRY_FILE,
        "sys/title/0005001b/1005c000/content/language.txt": LANGUAGE_FILE,
      "usr/",
        "usr/title/00050000/", // base
        "usr/title/0005000c/", // dlc
        "usr/title/0005000e/", // update
        // Mii Maker save folders {0x500101004A000, 0x500101004A100, 0x500101004A200}
        "usr/save/00050010/1004a000/user/common/db/",
        "usr/save/00050010/1004a100/user/common/db/",
        "usr/save/00050010/1004a200/user/common/db/",
    )?;

    storage.artifacts().brand("NINTENDO_WIIU", label.as_deref()).await?;

    Ok(Self { storage })
  }

  fn permissions(&self, path: &str) -> Permissions {
    permissions_for(path, LAYOUT)
  }

  fn is_complete(&self, completion_criteria: NandCompletion) -> bool {
    // TODO: verify against mtrees.
    match completion_criteria {
      NandCompletion::Minimal => requires_structure!(&*self.storage,
        "sys/",
          "sys/title/0005001b/1005c000/content/country.txt",
          "sys/title/0005001b/1005c000/content/language.txt",
        "usr/",
          "usr/title/00050000/",
          "usr/title/0005000c/",
          "usr/title/0005000e/",
          "usr/save/00050010/1004a000/user/common/db/",
          "usr/save/00050010/1004a100/user/common/db/",
          "usr/save/00050010/1004a200/user/common/db/",
      )
      .is_ok(),
      NandCompletion::Full | NandCompletion::Verified => false,
    }
  }

  async fn clone<St: Storage + 'static>(&self, storage: St) -> Result<Self, Error> {
    todo!()
  }
}
