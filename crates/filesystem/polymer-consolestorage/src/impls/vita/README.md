# Vita console storage

`VitaNand` implements `VirtualNand` for consolespec's `SONY_PSV` platform.
`ConsoleStorage` supports `Nand` and `SaveRepo` for this platform. The runtime's
existing `[saveRepo, nand]` composition can be selected as the `emmc` device;
the mounted root contains the Vita device directories (`ux0`, `vs0`, etc.).

`VitaSaveRepo` uses the existing `SonySaveRepo` and unchanged `SaveLayout` trait.
The guest user is always `00`. A Polymer save profile selects the save volume,
not a guest account. Saves live at `ux0/user/00/savedata`; the save volume owns
`ux0/user/00`, including trophy state. Save export/restore operates on a title's
savedata family, while other user state is retained by profile checkpoints.
Sony metadata lookup accepts both root-level PSP/PS3 files and Vita's
`sce_sys/param.sfo` and `sce_sys/icon0.png`.

Applications, patches, add-ons, and licenses belong to the system volume.
Installed content and firmware are read-only to the guest. The selected user's
directory, `ux0/data`, `uma0/data`, `vd0`, and `tm0` are writable. Data outside
the selected user's directory remains system-volume state rather than
per-title save history.

`VitaDirectoryDepot::open` accepts an already-decrypted directory containing
`sce_sys/param.sfo`. Categories `gd`, `gp`, and `ac` install under `ux0/app`,
`ux0/patch`, and `ux0/addcont`, respectively. Applications and patches require
`eboot.bin`; add-ons require a content ID whose title matches `TITLE_ID`.
Links and `sce_pfs` trees are rejected. Drive installation with
`nand.try_install(path, keys)?.finish().await`. Extraction uses a temporary
directory, and completion publishes the artifact and its files together.

Applications, patches, and each add-on directory have independent Git histories.
Each revision is a complete component snapshot. Installing a different version
replaces that component's files, removing files omitted by the replacement.
Installing its currently installed version again requires explicit uninstall
first. `storage.artifacts().history(id)` lists the active component's ancestry;
`restore(id, revision)` rolls it back to an ancestor without changing other
components. Revisions beyond the rollback point leave the active history.

Uninstalling an application reports its installed patch and add-ons through
`affected`; callers must include those artifacts in the removal request.
Patch removal and rollback preserve add-ons. Version-specific DLC requirements
are not inferred. A patch or add-on can be installed without an application in
the NAND, allowing a base title supplied by external media.

Component refs and the composed NAND checkout are published through a recovery
journal. Opening the volume or starting another operation completes a pending
transition before proceeding. Failed extraction leaves the installed NAND
unchanged; failures after journaling are completed on recovery. Ordinary guest
changes still require explicit NAND recovery before installation or removal.
Existing NANDs using the old global artifact history require a one-time
conversion outside the application; there is no automatic migration.

This backend does not extract firmware, archives, encrypted PKGs, or PFS data,
and does not install licenses. It does not add a Vita3K emulator plugin.
`Minimal` completion checks the layout and a nonempty firmware font library;
it is only a presence check. `Full` and `Verified` remain false.

Save identity follows the shared Sony product-code convention (four letters
and five digits, with a family containing directories sharing that prefix).
Nonstandard homebrew IDs and mapping an application's title ID to a different
savedata ID need additional identity support. Save management still enumerates
existing product-code savedata directories independently of launch metadata.

The native paths and content destinations follow Vita3K's
[filesystem](https://github.com/Vita3K/Vita3K/blob/master/vita3k/io/src/io.cpp)
and [package installer](https://github.com/Vita3K/Vita3K/blob/master/vita3k/packages/src/pkg.cpp).
