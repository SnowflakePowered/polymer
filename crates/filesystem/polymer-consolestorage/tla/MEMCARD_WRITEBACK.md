# Memory-card write-back verification (T16)

A memory card is mounted once per plugin session as one in-memory image. The guest's emulator writes into that image; Polymer's write-back parses it and stores each save canonically, and the next session's image is projected from the stored saves. Polymer cannot know where a guest's save begins or ends, and no surveyed emulator exposes it, so the stored card must be protected by checks Polymer can perform.

## Invariants

- `GuestViewConsistent` (guaranteed): every stored save is structurally valid, so any mount, including one after an app crash, shows a structurally valid card.
- `SessionEndConsistent` (guaranteed): the same, at the end of a session ended by an emulator crash or by `onQuit`.
- `EventuallyConsistent` (guaranteed): after a session ends, every save that is structurally valid in the final image is eventually stored.
- `StoredIsRealState` (best effort): every stored save is a state the guest actually produced, not a mix of its writes.

Structural validity is the card format's own consistency: directory entries, allocation tables, and sizes agree, and checksums match where the format has them. A game overwriting a save's data in place keeps the structure valid; that case is the game's responsibility, as it is on a real console after power loss.

## Implementation

Each card's write-back decodes saves individually through `RawMemoryCardImage::lenient_saves`, which yields each identity with its save, or none when the save is kept. A card whose own structure is invalid stores and removes nothing and returns success, so the worker waits for the next guest write instead of retrying. On a valid card, each save is written, kept, or removed:

- **Written:** the save decodes and passes its format's checks.
- **Kept:** its stored version is neither written nor removed. This covers broken or overlapping allocation chains, duplicated identities, sizes disagreeing with allocation, unparsable entries, and a save being created with no files yet.
- **Removed:** it has no directory entry.

Allocated blocks reachable from no save are ignored.

Every format then hands its decoded saves to one shared `WriteBack` (`impls/memorycard/mod.rs`) as entries of an identity, the stored paths it maps to, and its contents when it is written. The format decides only identity, paths, and validity: the GameCube region check, Dreamcast's product and VMI paths. `WriteBack` keeps, across the passes of one session, the paths the session is responsible for (those projected at mount plus every path written since) and each identity's last stored paths. Each pass:

1. Every entry claims its own paths and its identity's last stored paths.
2. An entry with contents is written only when no other entry claims any of its paths, so two saves mapping to one path, by duplicated identity or lossy path encoding, are both kept.
3. A path the session is responsible for that no entry claims is removed, so a save created and then deleted within one session does not survive it.

Files are compared before they are rewritten, for every format.

| Format | Card-level invalid | Notes |
|---|---|---|
| PS1 (`ps1/raw/card.rs`) | wrong size, header or directory-frame checksum | a path written in a pass is never removed by the same pass |
| PS2 raw (`ps2/raw/card/write.rs`) | superblock, FAT, or root directory unreadable | a directory entry with no files is kept rather than removed |
| PCSX2 folder (`ps2/pcsx2.rs`) | `_pcsx2_superblock` missing, invalid, or of the wrong capacity | `_pcsx2_meta_directory` and `_pcsx2_index` are not validity signals: PCSX2 deletes the former for standard saves and rewrites the latter as YAML |
| GameCube raw (`gamecube/raw/card.rs`) | both directory or both allocation-table copies fail their checksums; free-block count disagrees | a path written in a pass is never removed by the same pass |
| Dolphin GCI folder (`gamecube/dolphin.rs`) | — | a `.gci` that fails to parse, including a truncated one, is kept by its header identity and by the identity of the save projected at that file name |
| Dreamcast (`dreamcast/raw/card.rs`) | root block or directory chain unreadable | colliding VMI paths are kept; a path written in a pass is never removed by the same pass |

Strict parsers used by projection and export are unchanged. Thirteen public integration tests in `tests/{ps1,ps2,gamecube,dreamcast}_writeback_validity.rs` corrupt one save or the card structure through the guest view and check that sync succeeds, that a kept save's stored files are unchanged, and that valid saves are stored. Others cover a new save written to the path of a deleted one (PS1 and Dolphin), two valid saves sharing a path, and a save created and deleted within one session (PS1). Overlap, duplicate-identity, and Dreamcast collision branches are implemented but not each covered by a test.

## Model

`MemcardWriteback.tla` models two saves. A guest operation overwrites a save's two data fields, creates a save (structure torn until it closes), or deletes one, and may be abandoned midway. Guest writes land in the emulator's copy, which reaches Polymer's image according to `Order`: `inorder` write-through (PCSX2 raw, Flycast), `atomic` whole-image replacement (DuckStation), or `burst`, a non-atomic copy of a snapshot (Dolphin GCI, the PCSX2 folder, ares). A session ends by `Crash`, which stops publishing immediately, or by `Quit`, after which the emulator publishes everything and closes. `Pass` is an atomic write-back; `Policy = "validate"` stores only structurally valid saves, decoding a save without a directory entry as absent, and `"current"` stores every save. `QuietReads` forbids a pass during a burst, approximating the write-back quiet period. A later session mounts from the stored saves.

Results with TLC on 2026-10-09 (two saves, versions {1, 2}, three guest operations, two sessions):

| Configuration | Result | Distinct states |
|---|---|---:|
| `MCMemcardWriteback.cfg` (validate, burst, quiet reads, one crash) | `GuestViewConsistent`, `SessionEndConsistent`, `EventuallyConsistent` pass | 1,297,249 |
| `MCMemcardWritebackRealState.cfg` (same, no crash) | also `StoredIsRealState` passes | 430,263 |
| `MCMemcardWritebackInOrder.cfg` (write-through, one crash) | all four pass | 111,563 |
| `MCMemcardWritebackAtomic.cfg` (atomic replacement, one crash) | all four pass | 97,423 |
| `MCMemcardWritebackCrashMidBurst.cfg` | `StoredIsRealState` violated: a burst publishes a directory entry before its data, the emulator crashes, and the final pass stores a valid save the guest never wrote | — |
| `MCMemcardWritebackNoQuiet.cfg` (no crash, passes during bursts) | `StoredIsRealState` violated | — |
| `MCMemcardWritebackCurrent.cfg` (current policy) | `GuestViewConsistent` violated: a torn save is stored and later mounted | — |

The only remaining way to store a state the guest never produced is an emulator crash during a non-atomic burst; guarding against it is best effort. The model treats each write-back pass as atomic, a structural check as exact, and the quiet period as long enough to separate a burst from the pass that follows it.

## Path ownership

`MemcardPaths.tla` checks the `WriteBack` rule above, which `MemcardWriteback.tla` abstracts away by giving every save its own fixed slot. Three identities map to two paths, two of them colliding. A session mounts any subset of the stored files (capacity can leave saves off the card), the guest creates, overwrites, tears, or deletes saves, and passes run atomically. `TornPath` says whether a torn entry still knows its own path (PS1) or only its identity (a GameCube file that does not parse).

- `WrittenStored`: after a pass, every writable save is stored at its path.
- `DeletedGone`: after a pass, every path the session projected or wrote holds a file only if a save on the card claims it.
- `NoCollateral`: files the session neither projected nor wrote are untouched.
- `KeptUnchanged`: a pass never changes a path claimed by a save it did not write.

Results with TLC on 2026-10-09 (three identities, two paths, versions {1, 2}, four guest edits, two sessions):

| Configuration | Result | Distinct states |
|---|---|---:|
| `MCMemcardPaths.cfg` (claims, torn entries without paths) | all pass | 169,917 |
| `MCMemcardPathsTornPath.cfg` (torn entries with paths) | all pass | 188,909 |
| `MCMemcardPathsFixed.cfg` (responsibility fixed at mount) | `DeletedGone` violated: a save created and deleted in one session stays stored | — |
| `MCMemcardPathsUnguarded.cfg` (removal by identity only) | `WrittenStored` violated: a pass removes a path it just wrote | — |
| `MCMemcardPathsLastWins.cfg` (no collision check) | `WrittenStored` violated: one of two saves sharing a path is lost | — |

A probe invariant confirmed that a valid save kept because of a collision is reachable. The model fixes each identity's path; Dreamcast's product-dependent paths are covered only through `known`, which the implementation records per identity.
