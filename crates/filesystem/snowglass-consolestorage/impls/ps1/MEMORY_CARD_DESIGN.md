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
  opaque/free-block bytes if lossless raw round trips are required; save files alone are not a
  complete physical-card representation.

The debug converter and `ps1-memcard` dependency are not production code.

## Canonical representation

Use one directory per logical save. Only the save payload belongs in the depot:

```text
save-depots/
  <save-depot-id>/
    data.bin                # chained 8 KiB blocks in logical order
```

The manifest database maps the depot ID to the raw 20-byte filename, original directory entry,
allocation order, and other console metadata. The raw filename must not be used directly as a
Windows path because it can contain names which are invalid or ambiguous on the host filesystem.

An opaque physical template in the manifest database preserves formatted free blocks,
deleted-save residue, broken-sector metadata, and unknown bytes. It is codec state, not the logical
source of truth: projection overlays the current canonical saves onto it. Reconciliation refreshes
it only after the raw image parses as a coherent card. This gives exact no-change round trips
without making a monolithic image the user-facing save representation.

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

1. Acquire an exclusive card lease and snapshot the selected depots.
2. Encode them over the preserved template into a 131,072-byte random-access image.
3. Expose that image as `snowglass.mcd` through a VFS `FileNode`.
4. Accept arbitrary reads, writes, truncation, and atomic sibling-file replacement while tracking
   dirty generations.
5. On flush, cleanup, replacement, or view finalization, validate and decode a coherent image.
6. Atomically reconcile added, changed, and deleted logical save directories, then publish normal
   storage change notifications.

DuckStation currently saves the whole card via atomic replacement rather than in-place sector
writes. The VFS must adopt the renamed source file object as the mounted card's backing object and
transfer the `MemoryCardView` dirty/sync observer to its existing and future handles. Copying bytes
at rename is insufficient because an already-open source handle may legally continue writing after
the rename. Watching only writes to the original destination node would miss the save.

Directory-side changes take the reverse path. When no raw writer owns the view, a depot change
invalidates the encoded generation and the next open/read rebuilds it. Concurrent raw and
directory writers are intentionally unsupported; transparency is guaranteed across flush/close
and subsequent launches.

## Plugin integration

The DuckStation plugin now boots a cue/bin game tree, BIOS, portable settings, and a shared raw
card through WinFSP. It temporarily mounts the supplied NAND at `/User/memcards`. Once the storage
capability exists, it should request the raw projection explicitly:

```ts
args.filesystem.mountMemoryCard("/User/memcards/snowglass.mcd", { format: "raw" });
```

DuckStation continues to see the same path; only the plugin capability changes.

The same PS1 view can be passed to RetroArch cores that accept raw `.mcd`/`.srm` cards. No
emulator-specific conversion is required after the PS1 adapter produces the view.
