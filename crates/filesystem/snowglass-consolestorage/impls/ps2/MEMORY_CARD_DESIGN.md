# PS2 directory-backed memory-card views

## Decision

A save-depot directory can be the canonical representation of a PS2 memory card while a raw
`.ps2` image is projected for emulators which do not support folder cards. The projection can be
served through Snowglass's WinFSP namespace; it does not require a native staging path.

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

## PS2 representations

The PCSX2-compatible canonical tree is:

```text
Mcd001.ps2/
  _pcsx2_superblock             # first 8 KiB erase block, without spare/ECC bytes
  <save-directory>/
    _pcsx2_meta_directory       # raw 512-byte directory entry
    _pcsx2_index                # file order and timestamps
    _pcsx2_meta/
      <file>                    # optional raw 512-byte file entry
    <file>                      # save payload
```

The LRPS2 view is a raw image with 16,384 pages of 512 data bytes plus 16 spare/ECC bytes per
page. An 8 MiB card is therefore 8,650,752 bytes on disk.

The production PS2 adapter must implement the Sony superblock, indirect FAT, FAT chains,
directory entries, backup blocks, ECC, PCSX2 filename escaping, and `_pcsx2_index` ordering. File
order is observable by games and cannot be replaced with host directory iteration.

## Proposed interfaces

`MemoryCard` owns the canonical save-depot storage and grants an exclusive view lease:

```rust
pub trait MemoryCard: Storage {
    type Format: MemoryCardFormat;

    fn expose(
        &self,
        primary: &[SaveDepotId],
        capacity: MemoryCardCapacity,
    ) -> Result<MemoryCardView<Self::Format>, Error>;
}
```

`MemoryCardView` is a fixed-length random-access image, independent of VFS and WinFSP:

```rust
pub trait RandomAccessImage: Send + Sync {
    fn len(&self) -> u64;
    fn read_at(&self, offset: u64, output: &mut [u8]) -> Result<usize, Error>;
    fn write_at(&self, offset: u64, input: &[u8]) -> Result<usize, Error>;
    fn flush(&self) -> Result<(), Error>;
}
```

The VFS crate supplies a small `FileNode` adapter over `RandomAccessImage`. This avoids making
console storage depend on WinFSP or on the VFS implementation. An mmap is an implementation
choice for the image buffer, not part of the contract; an owned page cache is also valid.

`MemoryCardFormat` is console-specific and operates on logical snapshots:

```rust
pub trait MemoryCardFormat: Send + Sync {
    fn geometry(&self, capacity: MemoryCardCapacity) -> Geometry;
    fn encode(&self, saves: &SaveDepotSnapshot, image: &mut dyn ImageWriter)
        -> Result<(), Error>;
    fn decode(&self, image: &dyn ImageReader) -> Result<SaveDepotSnapshot, DecodeError>;
}
```

PS1, PS2, Dreamcast VMU, and GameCube cards can share the view, dirty-range tracking, leases, and
VFS adapter while supplying different format implementations.

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
3. Implement PCSX2 folder metadata and filename translation, with fixtures for ordering,
   timestamps, nonstandard modes, empty files, deletion, and full-card behavior.
4. Add `MemoryCardView` reconciliation and single-writer leases.
5. Pass the same memory-card capability to PCSX2 as a directory mount and to LRPS2 as
   `system/pcsx2/memcards/Mcd001.ps2`.
6. Add process-level round-trip tests: PCSX2 folder write to LRPS2 raw read, then LRPS2 raw write to
   PCSX2 folder read.
