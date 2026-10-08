# Wii component histories

Each WAD title has an independent history of complete content snapshots. The
component owns `title/<high>/<low>/content`; sibling `data` directories remain
outside it. Installing another version replaces the selected content snapshot.
Rollback selects an ancestor. Installing the currently selected version again
requires removing it first.

Extraction writes every decrypted content into staging under its title root.
The Wii composer maps shared content into `shared1`, deduplicates by SHA-1, and
rejects matching SHA-1 claims with different BLAKE3 hashes or lengths. It generates
`shared1/content.map` from the selected titles. A shared file survives while any
selected title needs it. Unowned destination files cause a conflict.

The aggregate commit stores opaque composer state alongside component selections.
Wii uses it to preserve UID and shared-content-number allocations across updates,
rollback, removal, and reinstall. Removed titles keep their UID reservations.
The UID table, shared-content map, files, artifacts, and selections publish through
the same recovery journal.

The TMD's system-version field records the required IOS. Removing an installed IOS
reports titles that reference it. Installation does not require the IOS already
to be present, allowing system software to be assembled in either order. IOS
versions are not inferred from the title ID. DLC titles are content artifacts,
without launch entrypoints; a DLC-to-game dependency is not guessed from its ID.

WAD contents and TMD identity are checked during staging, including changes to
the TMD after opening and duplicate content IDs or indices. Tickets remain in the
key store. Failed extraction and cancellation do not modify the installed NAND.
Failures after journaling are completed on recovery.

Native formats and allocation behavior follow Dolphin's
[title and shared-content structures](https://github.com/dolphin-emu/dolphin/blob/master/Source/Core/Core/IOS/ES/Formats.cpp)
and [WAD installation](https://github.com/dolphin-emu/dolphin/blob/master/Source/Core/Core/WiiUtils.cpp).
