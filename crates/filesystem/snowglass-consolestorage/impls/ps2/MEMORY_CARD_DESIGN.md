# PS2 save-depot memory-card projections

## Decision

A set of save-depot directories is the canonical representation of the saves on a PS2 memory
card. Those directories contain only console-visible save directories and files. PCSX2 folder-card
bookkeeping such as `_pcsx2_superblock`, `_pcsx2_meta_directory`, `_pcsx2_index`, and
`_pcsx2_meta` is projection metadata and must not be materialized in a save depot.

Projection metadata belongs in the manifest database. A plugin asks for the representation its
emulator consumes; the memory-card capability owns translation and metadata:

```ts
args.filesystem.mountMemoryCard("/User/memcards/Mcd001.ps2", { format: "pcsx2" });
args.filesystem.mountMemoryCard("/User/system/pcsx2/memcards/Mcd001.ps2", { format: "raw" });
```

Both projections can be served through Snowglass's WinFSP namespace and operate on the same
canonical depots. Neither requires a native staging path.

The reusable `MemoryCardView` should be console-neutral. Encoding a directory as an image,
validating image transactions, and reconciling them back into save depots must be supplied by a
console-specific format adapter.

## Experimental evidence

- PCSX2 opened a save-depot-backed directory through WinFSP as
  `McdSlot 0: [Folder] .../Mcd001.ps2` and indexed it with its folder-card backend.
- LRPS2 opened a pre-existing 8 MiB PS2 card dump through WinFSP with read/write access and read
  the complete 8,650,752-byte image.
- A debug-only converter built with `ps2-memcard` projected a synthetic save from one PCSX2 folder
  to a raw image and back. Changing a file in the folder appeared in the next raw image; changing
  it in the raw image appeared in the same canonical folder after reconciliation.
- LRPS2 initially failed to discover the BIOS because the plugin mounted its directory with `r`
  rather than `rx`. Snowglass therefore rejected directory enumeration. With `rx`, LRPS2 found
  and read the BIOS through the same namespace. This was not a WinFSP limitation.

The debug converter and `ps2-memcard` dependency remain outside production code.

## Canonical storage and projections

Canonical storage has no emulator protocol files:

```text
save-depots/
  <save-depot-id>/
    <save-directory>/
      <game file>
```

The `pcsx2` projection is a directory view. It synthesizes PCSX2's reserved files from manifest
records and maps ordinary save files to the canonical depots. Writes to reserved files update
manifest records; writes to game files update the corresponding depot.

The `raw` projection is a PS2 card image with 16,384 pages of 512 data bytes plus 16 spare/ECC
bytes per page. An 8 MiB card is therefore 8,650,752 bytes on disk. Its superblock, FAT, directory
entries, backup blocks, and ECC are encoded from the same depot snapshot and manifest metadata.

The production PS2 adapter must implement the Sony superblock, indirect FAT, FAT chains,
directory entries, backup blocks, ECC, and PCSX2 filename escaping. File order, timestamps, modes,
and unknown entry bytes are observable and cannot be replaced with host directory iteration; they
are persisted in the manifest database.

## Manifest metadata

The existing artifact manifest tables should not be overloaded. Add memory-card tables owned by
console storage, conceptually:

```text
memory_cards(card_id, platform, capacity, generation)
memory_card_saves(card_id, save_depot_id, ordinal, console_name)
memory_card_metadata(card_id, save_depot_id?, namespace, key, value, generation)
```

`memory_card_saves` establishes card membership and stable ordering. `memory_card_metadata` holds
console semantics which are not representable by host files, such as raw names, timestamps, modes,
opaque directory-entry bytes, allocation hints, and the state surfaced by PCSX2's reserved files.
The `namespace` identifies the codec which owns a record; it does not create a second source of
truth for a projection. Both the `pcsx2` and `raw` codecs read and update the same PS2 metadata.
All records are versioned with the depot snapshot so stale metadata is never applied to newer save
data.

The database is authoritative for metadata, while save depots are authoritative for game payloads.
Projection files are disposable views and can always be regenerated from those two sources.

## Proposed interfaces

`MemoryCard` owns the canonical save-depot storage and manifest records. It grants an exclusive,
rooted storage view in a requested format:

```rust
pub trait MemoryCard: Storage {
    fn view(
        &self,
        primary: &[SaveDepotId],
        capacity_blocks: u32,
        format: &str,
    ) -> Result<MemoryCardView, Error>;
}
```

Every view is a storage namespace plus the entry within it that the emulator mounts:

```rust
pub struct MemoryCardView {
    storage: Arc<dyn MountableStorage>,
    entry: String,
}
```

`entry` is a validated relative storage path. Its metadata identifies the mount shape:

- `raw` names a fixed-length seekable file within the namespace.
- `pcsx2` names the namespace root (`""`), which is a synthetic directory tree.

There is no `MemoryCardProjection`, `ProjectedTree`, or `RandomAccessImage` public abstraction.
`StorageFile` already supplies read, write, and seek; `MountableStorage` adds rename, truncate, and
sync. An mmap or owned page cache remains an implementation detail of the raw storage.

An emulator may write a temporary sibling in another mounted storage and atomically rename it over
the raw card. The composed VFS must adopt that source file object into the mounted card instead of
copying its bytes. The mounted path, already-open source handles, and subsequent target opens then
refer to the same object. The card's dirty/sync observer is transferred to the adopted object so
post-rename writes and flushes still reach reconciliation. The displaced raw projection remains
alive only for handles which were already open to it, matching normal replacement semantics.

The final view `Arc` is the card lease. Its implementation owns the opening snapshot, dirty state,
and reconciliation boundary. Dropping its final reference performs the last best-effort
reconciliation; explicit `sync` remains the observable commit boundary.

Format providers are console-specific and operate on logical snapshots plus manifest metadata:

```rust
pub trait MemoryCardFormat: Send + Sync {
    fn id(&self) -> &str;
    fn open(&self, session: MemoryCardSession)
        -> Result<MemoryCardView, Error>;
}
```

`MemoryCardSession` is the common internal capability containing the exclusive writer lease,
opening snapshot, manifest metadata, and atomic reconcile operation. A format implementation owns
the storage operations and console codec which use that session. PS1, PS2, Dreamcast VMU, and
GameCube cards therefore share session and commit machinery without forcing file and directory
formats through different public interfaces.

Artifact-registry, volume identity, and branding operations live on the
`VolumeStorage: MountableStorage` extension. User-facing `label` and `set_label` remain on
`Storage`, so canonical memory cards can be named without pretending to be managed volumes.
Memory-card views implement only `MountableStorage` and forward the card label where appropriate.

At the plugin boundary, the launch supplies exactly one authorized memory-card capability.
`mountMemoryCard` accepts only a target path and projection format:

```ts
mountMemoryCard(target: string, options: { format: "raw" | "pcsx2" }): void;
```

The operation asks the assigned card to resolve the format, looks up the view's `entry`, and mounts
it as either a VFS file or tree. Replacement of a mounted raw file is a specific composed-VFS
operation: detach the source file object from its parent volume, adopt it as the mounted file's
backing object, and attach the memory-card observer to its existing and future handles. It is not a
general cross-volume rename. A plugin cannot name a card, depot, or host path, and requesting an
unsupported format fails during launch setup. `raw` is a common format identifier whose codec is
selected by the card's platform; `pcsx2` is available only from the PS2 provider.

## Write synchronization

Raw writes cannot be translated independently. A single memory-card save rewrites erase blocks,
FAT entries, backup blocks, directory entries, payload pages, and ECC in intermediate states.
Attempting to map each callback directly to a depot file would expose corrupt partial
transactions.

The view should instead:

1. Take a versioned snapshot of the selected depots and encode it into the raw image when the view
   opens.
2. Apply emulator writes to the image and record dirty erase blocks.
3. On flush, handle cleanup, or view finalization, ask the format adapter to validate and decode a
   coherent filesystem. If validation fails because a transaction is incomplete, retain the raw
   bytes and retry after later writes; do not modify the canonical directory.
4. Diff the decoded logical saves against the opening snapshot and atomically reconcile complete
   save directories, metadata, additions, and deletions into storage.
5. Publish the normal storage change signal only after reconciliation. `SaveGameSink::watch_saves`
   then observes ordinary depot changes.

Directory-side changes use the reverse path. A storage watcher invalidates the encoded image; the
next read/open rebuilds it when no raw transaction is dirty. Rebuilding while a raw writer owns the
view is deferred.

The card needs a single-writer lease. PCSX2's folder backend caches the synthesized FAT and keeps
save files open, while LRPS2 independently caches a raw filesystem. Running both as writers at the
same time cannot provide meaningful last-write-wins behavior. Transparency is guaranteed across
flush/close and subsequent launches, not between two concurrent emulator processes.

## Implementation order

1. Add the VFS adapter for a `MemoryCardView` storage entry, including source-node adoption,
   open-handle observer transfer, and finalize-on-lease behavior.
2. Implement the PS2 parser/builder in Snowglass, using the debug crate only as a test oracle.
3. Add manifest tables and transactions for console metadata and format-specific projection state.
4. Add the pluginhost `mountMemoryCard(path, { format })` capability and resolve the requested
   format against the invocation-assigned card.
5. Implement the PCSX2 directory projection without writing reserved `_pcsx2_*` files into save
   depots, with fixtures for ordering, timestamps, nonstandard modes, empty files, deletion, and
   full-card behavior.
6. Add `MemoryCardView` reconciliation and single-writer leases.
7. Pass the same memory-card capability to PCSX2 as a directory mount and to LRPS2 as
   `system/pcsx2/memcards/Mcd001.ps2`.
8. Add process-level round-trip tests: PCSX2 folder write to LRPS2 raw read, then LRPS2 raw write to
   PCSX2 folder read.
