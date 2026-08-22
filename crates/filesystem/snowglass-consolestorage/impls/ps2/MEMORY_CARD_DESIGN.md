# PS2 save-depot memory-card projections

## Decision

Each console-visible save is stored canonically as one EMS Adapter PSU container:

```text
card/
  BASLUS-21782Save0.psu
  BASLUS-21782Save1.psu
```

The PSU contains the save directory entry, its file directory entries, their raw names and
metadata, and all payload bytes. There is no sidecar manifest and no emulator-specific data in
canonical storage. The native PS2 save name is authoritative and determines the PSU filename.

The PSU codec's `psu::Save<'_>` is a zero-copy view over `Cow<'_, [u8]>`. A PSU read borrows the
input buffer; encoding a structured save produces `Save<'static>`. Fixed-layout 512-byte directory
entries are validated and viewed with `zerocopy`. File payloads are borrowed slices and begin after
their directory entries, with the format's 1024-byte alignment. `psu::Save` is also the structured
contract used by the canonical card and both projections.

## Projections

The `raw` projection builds a full PS2 NAND image from the selected PSU saves. Emulator writes are
retained in the view until they form a coherent filesystem, decoded into structured saves, encoded
as complete PSU files, and atomically published with `atomic-write-file`.

The `pcsx2` projection expands the same PSU saves into an in-memory folder card. It synthesizes
`_pcsx2_superblock`, `_pcsx2_meta_directory`, `_pcsx2_index`, and `_pcsx2_meta/*`; these files never
reach canonical storage. Ordinary file and PCSX2 metadata writes are decoded back into a complete
save and atomically replace its PSU on every write/sync boundary.

Both projections therefore preserve the raw PS2 directory metadata without `._dirent`, while
`SaveGameSink::watch_saves` observes ordinary atomic changes under `card/`.

## Concurrency

A view owns the set of saves it projected plus saves created through that view. Reconciliation can
add, replace, or delete only that owned set, so capacity filtering cannot delete saves that were not
projected. Concurrent emulator writers still require a higher-level single-writer lease; emulator
filesystem caches do not provide meaningful cross-process last-write-wins behavior.

The raw projection uses the native zero-copy superblock, FAT, directory-entry, and PSU codecs. It
copies only when materializing a raw card or publishing an owned PSU; parsing borrows directly from
the current image.
