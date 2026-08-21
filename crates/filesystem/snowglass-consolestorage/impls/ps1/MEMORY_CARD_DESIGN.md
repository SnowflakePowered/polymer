# PS1 save-depot memory-card projection

## Decision

The design works for PS1. A directory of canonical save depots can be projected as the fixed
128 KiB raw card required by DuckStation and other emulators. DuckStation does not provide a
native folder-card mode, so the directory is a Snowglass storage format rather than an emulator
format.

`MemoryCardView` should remain console-neutral. PS1 directory entries, block chains, formatting,
and reconciliation belong in a PS1 format adapter. The view owns the raw, fixed-size byte buffer,
dirty-range tracking, flush/finalization boundaries, and the exclusive writer lease.

## Experimental evidence

The experiment used DuckStation 0.1-11752 and Crash Bandicoot (USA), identified by DuckStation as
`SCUS-94900`.

- A debug-only `ps1-memcard` converter projected a canonical synthetic Crash save into a valid
  131,072-byte card. DuckStation loaded the card through `\\Snowglass`, reading the complete image
  in one operation.
- DuckStation found the BIOS and launched Crash entirely through the same WinFSP namespace.
- DuckStation's own memory-card editor formatted and saved the projected card through WinFSP. Its
  observed host transaction was: create sibling `snowglass.mcd.<nonce>`, write 131,072 bytes at
  offset zero, then rename it over `snowglass.mcd` with replacement enabled.
- The resulting raw card decoded to zero saves. Reconciliation removed the synthetic canonical
  save directory, and projecting that directory again produced a valid zero-save card.
- The debug builder normalized unused data blocks to zero while DuckStation formats them to
  `0xFF`, so the two valid zero-save images were not byte-identical. Production must preserve
  opaque/free-block bytes for the lifetime of a mounted view. Lossless physical-card persistence
  across mounts is explicitly not a requirement; save files are the canonical representation.

The debug converter and `ps1-memcard` dependency are not production code.

## Canonical representation

Store each native PS1 save file directly beneath the canonical card directory:

```text
card/
  BASLUS-00838 BBLIT       # chained 8 KiB blocks in logical order
```

The native filename is authoritative, so the filesystem is the complete source of truth and no
save registry is required. `SaveDepotId` is derived from its standardized 12-byte key/product
header. Title and icon are parsed from the first data block. The manifest database retains only
the storage device's identity and label; it does not contain save ordering or a raw card image.

Free blocks, deleted-save residue, broken-sector metadata, and other physical bytes live only in
the mounted in-memory view. They survive arbitrary emulator writes and atomic replacement for that
mount, but are deterministically formatted again on the next projection. They are not logical save
data and are not persisted as a second canonical representation.

## PS1 adapter responsibilities

A PS1 adapter must implement:

- the 128-byte frame and 8 KiB block geometry;
- the 15 directory entries in block zero;
- first/middle/last block-chain states and link fields;
- frame XOR checksums and formatted-card metadata;
- allocation, deletion, full-card handling, and preservation of opaque blocks;
- stable mapping between raw filename bytes and canonical depot IDs;
- validation that every active chain is bounded, acyclic, and references compatible entries.

The PS1 adapter is substantially smaller than the PS2 adapter, but it is still console-specific.
`MemoryCardView` should not know about frames, filenames, checksums, or directory entries.

## Runtime and synchronization

The raw file is the transaction surface while an emulator owns the card:

1. Enumerate depots and place requested primary saves first.
2. Fill remaining capacity with other saves in deterministic native-ID order.
3. Encode them into a fresh 131,072-byte in-memory random-access image.
4. Expose that image as `snowglass.mcd` through a VFS `FileNode`.
5. Accept arbitrary reads, writes, truncation, and atomic sibling-file replacement while tracking
   dirty generations.
6. On flush, cleanup, replacement, or view finalization, validate and decode a coherent image.
7. Reconcile added, changed, and deleted logical save directories, then publish normal
   storage change notifications.

DuckStation currently saves the whole card via atomic replacement rather than in-place sector
writes. The VFS must adopt the renamed source file object as the mounted card's backing object and
transfer the `MemoryCardView` dirty/sync observer to its existing and future handles. Copying bytes
at rename is insufficient because an already-open source handle may legally continue writing after
the rename. Watching only writes to the original destination node would miss the save.

Directory-side changes are picked up by the next mount. Concurrent raw and directory writers are
intentionally unsupported; transparency is guaranteed across flush/close and subsequent launches.

The raw memory-card view keeps its bytes in a `Cow`, borrowing mounted images and owning newly
formatted projections. `zerocopy` directory, block, title, palette, icon, and save views borrow
from the card on demand, so the card stores no self-references and parsing requires no path,
temporary file, or payload copy.

## Plugin integration

The DuckStation plugin boots a cue/bin game tree, BIOS, portable settings, and a shared raw card
through WinFSP. It mounts the supplied memory-card storage directly at the expected file and
requests the raw projection explicitly:

```ts
args.filesystem.mountStorage("/User/memcards/snowglass.mcd", args.storage.card1, {
  mode: "rw",
  format: "raw",
});
```

DuckStation sees an ordinary `.mcd` file while Snowglass owns its in-memory projection.

The same PS1 view can be passed to RetroArch cores that accept raw `.mcd`/`.srm` cards. No
emulator-specific conversion is required after the PS1 adapter produces the view.
