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

`MemoryCard` owns the canonical save-depot storage and manifest records. It grants an exclusive
view lease in a requested projection format:

```rust
pub trait MemoryCard: Storage {
    fn project(
        &self,
        primary: &[SaveDepotId],
        capacity: MemoryCardCapacity,
        format: &ProjectionFormatId,
    ) -> Result<MemoryCardProjection, Error>;
}
```

The lease and reconciliation machinery is common, but a projection may be a file or a directory:

```rust
pub enum MemoryCardProjection {
    Image(MemoryCardView<dyn RandomAccessImage>),
    Directory(MemoryCardView<dyn ProjectedTree>),
}
```

`raw` produces `Image`; `pcsx2` produces `Directory`. Both interfaces remain independent of VFS
and WinFSP. The VFS crate supplies `FileNode` and tree adapters around them.

The raw image interface remains fixed-length random access:

```rust
pub trait RandomAccessImage: Send + Sync {
    fn len(&self) -> u64;
    fn read_at(&self, offset: u64, output: &mut [u8]) -> Result<usize, Error>;
    fn write_at(&self, offset: u64, input: &[u8]) -> Result<usize, Error>;
    fn flush(&self) -> Result<(), Error>;
}
```

An mmap is an implementation choice for the image buffer, not part of the contract; an owned page
cache is also valid.

Format providers are console-specific and operate on logical snapshots plus manifest metadata:

```rust
pub trait MemoryCardProjectionFormat: Send + Sync {
    fn id(&self) -> &ProjectionFormatId;
    fn project(&self, saves: &SaveDepotSnapshot, metadata: &ProjectionMetadata)
        -> Result<MemoryCardProjection, Error>;
    fn reconcile(&self, projection: &MemoryCardProjection)
        -> Result<(SaveDepotSnapshot, ProjectionMetadata), DecodeError>;
}
```

PS1, PS2, Dreamcast VMU, and GameCube cards can share the view, dirty-range tracking, leases, and
VFS adapter while supplying different format implementations.

At the plugin boundary, the launch supplies exactly one authorized memory-card capability.
`mountMemoryCard` accepts only a target path and projection format:

```ts
mountMemoryCard(target: string, options: { format: "raw" | "pcsx2" }): void;
```

The operation asks the assigned card to resolve the format. It mounts an image as a VFS file and a
directory projection as a VFS tree. A plugin cannot name a card, depot, or host path, and requesting
an unsupported format fails during launch setup. `raw` is a common format identifier whose codec is
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

1. Add the VFS `FileNode` adapter for a fixed-size `RandomAccessImage`, including finalize-on-lease
   behavior.
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
