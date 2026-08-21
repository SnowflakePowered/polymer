# GameCube save-depot memory-card projections

## Decision

The save-depot-backed design works for GameCube memory cards. Canonical storage contains one
payload file per console save; GameCube directory-entry and card metadata live in the manifest
database. The same state can be projected either as Dolphin's GCI folder or as a raw GameCube
memory-card image:

```ts
args.filesystem.mountMemoryCard("/MemoryCard/USA", { format: "gci" });
args.filesystem.mountMemoryCard("/MemoryCard/MemoryCardA.USA.raw", { format: "raw" });
```

`gci` is a directory projection and `raw` is a file projection. Both can be served through the
WinFSP namespace without a native staging path.

## Experimental evidence

The experiment used Dolphin and Super Smash Bros. Melee (`GALE`) through Snowglass's WinFSP
namespace.

- Dolphin enumerated the projected GCI directory, read the 64-byte directory entry at offset 0,
  then read the 90,112-byte save payload at offset 64.
- A payload-only canonical depot plus manifest metadata reconstructed the exact original GCI file,
  including the same SHA-256 digest.
- The debug codec projected that GCI into a 16 MiB raw card with valid header, redundant directory,
  BAT checksums, allocation chain, and 2,032 free blocks. Dolphin opened that projection and read
  the complete 16,777,216-byte image through WinFSP.
- Melee created an 11-block save on a fresh raw card. Dolphin flushed the complete 16 MiB card as a
  single offset-zero write through WinFSP after its delayed flush interval. Decoding that image,
  rebuilding it, and decoding it again preserved the extracted GCI exactly.
- Dolphin's GCI folder implementation opens a dirty save with `wb`, writes the 64-byte entry followed
  by its 8 KiB payload blocks, and represents deletion by renaming the file with a `.deleted`
  suffix. A targeted Melee menu automation did not trigger a new folder write, so this transaction
  shape comes from Dolphin's implementation rather than that unsuccessful input attempt.

The debug codec and tracing changes are experiment-only and are not production dependencies.

## Canonical representation

Only console save payloads belong in save depots:

```text
save-depots/
  <save-depot-id>/
    data.bin
```

The manifest database owns the associated card and directory metadata:

```text
memory_cards(
  card_id, platform, capacity_mbits, encoding, format_time,
  serial, sram_bias, sram_language, dtv_status, generation
)

memory_card_saves(
  card_id, save_depot_id, ordinal, game_code, maker_code,
  console_name, modification_time, image_offset, icon_format,
  animation_speed, permissions, copy_counter, block_count,
  comments_address, opaque_dirent, generation
)
```

The exact schema may store the structured fields and opaque bytes together, but there must be one
authoritative record. A raw directory entry and the header bytes prefixed to a GCI file are two
encodings of that same record.

The card header must remain stable. Its serial is observable by serial-protected saves such as
F-Zero GX and Phantasy Star Online. Dolphin's optional `MC_SYSTEM_AREA` GCI-folder file is projection
state derived from this manifest record and must not appear in a save depot.

The redundant directory blocks, redundant BATs, physical block chains, free-block count, and their
checksums are raw projection state. They are regenerated from the ordered save snapshot. When a raw
write is reconciled, the decoder uses them to recover logical state but does not make physical block
numbers canonical.

## Projection codecs

### `gci`

The GCI directory projection exposes one `.gci` file per save:

```text
<maker>-<game>-<console-name>.gci
  0x0000..0x003f  synthesized 64-byte directory entry
  0x0040..        save-depot payload in 8 KiB blocks
```

The host filename is a reversible, escaped projection label, not identity. Identity is the game
code, maker code, and console filename in the manifest. Duplicate identities must be rejected even
when host filenames differ.

A GCI write can be reconciled independently after close or flush when its length equals
`64 + block_count * 8192`. The view parses and validates the directory entry, atomically replaces
the corresponding depot payload and manifest record, then publishes the normal save change signal.
A `.deleted` rename removes card membership and the canonical save only when the emulator commits
the deletion transaction.

### `raw`

The raw projection is a fixed-size random-access file. Valid card capacities are 4, 8, 16, 32, 64,
and 128 Mbit, corresponding to 59, 123, 251, 507, 1,019, and 2,043 usable blocks. Each physical
block is 8 KiB; a 128 Mbit card is therefore 16,777,216 bytes.

Projection encodes:

1. the stable card header;
2. two directory copies;
3. two block-allocation-table copies;
4. allocated payload block chains; and
5. erased free blocks.

Raw writes cannot be mapped to depots per callback. Dolphin may buffer card changes and rewrite the
whole image. The view applies writes to a private image buffer, tracks dirty ranges, and on flush or
finalization validates both checksums and the active directory/BAT pair. Active redundant copies
are selected using the GameCube signed update-counter rules. Only a coherent logical snapshot is
atomically reconciled into depots and manifest metadata.

## Shared storage view

The public view contains an `Arc<dyn MountableStorage>` namespace and the relative entry to mount.
For `gci`, that entry is the directory root; for `raw`, it is the card file. If an emulator replaces
the mounted raw file with a sibling from another storage, the composed VFS adopts the source file
object and transfers the view's dirty/sync observer rather than copying bytes. There is no separate
`MemoryCardProjection` or `ProjectedTree` abstraction. The storage owns the snapshot generation,
exclusive writer lease, dirty state, and reconciliation boundary. The GameCube provider owns GCI
headers, card geometry, redundant metadata selection, checksums, BAT chains, filename escaping,
and serial-protected-save handling.

An mmap is useful for the raw file but is not part of the contract. The GCI storage exposes
synthetic `StorageFile` handles backed by depot payloads and manifest records.

Directory-to-raw and raw-to-directory transparency is guaranteed across validated flushes and
subsequent launches. Two simultaneous emulator writers still require a single-writer lease.

## Implementation order

1. Add the GameCube card and save metadata records to the manifest database, including a stable
   card header and generation.
2. Implement GCI parsing/building and the `gci` `MountableStorage` provider.
3. Implement raw header, directory, BAT, checksum, and allocation codecs with fixtures for every
   supported capacity and redundant-copy recovery.
4. Add GameCube format resolution to `mountMemoryCard(path, { format })` and update Dolphin to mount
   `gci` instead of the temporary scratch tree. Format selection belongs to the emulator plugin;
   it is not inferred from the path or canonical depot.
5. Reconcile GCI files independently and raw images transactionally, publishing save changes only
   after atomic commit.
6. Add process-level tests for GCI write to raw read, raw write to GCI read, deletion, card-full
   behavior, filename collisions, damaged redundant copies, and serial-protected saves.
