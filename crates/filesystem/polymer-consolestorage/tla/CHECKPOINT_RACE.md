# Save checkpoint race verification (T15)

Commits are the only save checkpoints, and crash recovery rolls back to the last one, so a commit must never capture a file mid-write. Guest writes reach the checkout in place while checkpoints run, and the previous length-and-mtime recheck let a commit read a file whose write was still in progress.

## Implementation

- **Per-path activity.** `GitVolumeInner` keeps, per path, the number of write calls in progress and a generation that increases after every write call and namespace change (`volume.rs`, `traits.rs`). `GitVolumeFile::write_at` and `set_len` hold a `WriteCall` guard for the call's duration. Starting the guard marks the path pending again under the working-state lock, which a commit holds from its activity re-check until the index is written, so a write through a handle that stays open after a commit is staged by the next checkpoint and cannot slip between the re-check and the index (`tests/open_handle_checkpoint.rs`). A write never replaces a pending deletion, and a pending file that no longer exists, such as the old name of a staging file renamed while open, is dropped at the next commit. Writes through a handle whose file was renamed still mark the old name, so after the rename's commit they are only picked up by the next write through a handle on the new name. Mapped writes reach Polymer as paging writes through the same path.
- **Scoped commits.** `commit_scoped` commits only pending changes within a `PendingScope` (`All`, `Within(root)`, `Except(roots)`). A file is committed only if no write was in progress when it was staged and its generation is unchanged after staging; otherwise it stays pending. Files left pending have a zeroed index stat, so crash recovery compares their content instead of trusting the index (`commit.rs`, `checkout.rs`).
- **Per-save checkpoints.** `SaveVersioning::checkpoint_all` checkpoints each save within its own `root_path`. A save is deferred if a write under its root is in progress, if its activity changes while it is exported, or if the gate rejects it. Each save's profile commit is limited to its root; the final profile commit excludes every deferred root. A tracked save that disappeared is gated with an empty root before it is archived.
- **Memory-card write-back passes.** A reconcile pass installs each canonical save file atomically but writes a multi-file save (a PS2 save directory, a Dreamcast VMS and VMI) one file at a time. Each card's write-back function holds a `WritePass` on its volume for the whole pass, which counts as an in-progress write for every save on the volume, so a checkpoint started during the pass, or overlapping it, defers (`ps1`, `ps2`, `gamecube`, `dreamcast` `reconcile`).
- **Plugin gate.** `SaveRepo::checkpoint_saves_gated` takes an optional `CheckpointGate`, a boxed function from `CheckpointRequest` (save, root, and the root's pending changes) to a future `bool`. It is asked only when the save has pending changes. `polymer-consolestorage` has no Deno dependency. The runtime's `CheckpointSession` (`polymer-runtime/src/storage.rs`) builds a gate per storage name and, once the launch's controller session exists, sends `SaveCheckpoint` to its `ControllerActor`, which calls the plugin's optional `onSaveCheckpoint` export with a one-second deadline. A missing export, a missing session, an error, or a timeout counts as ready. The baseline checkpoint at launch and the final checkpoint after exit do not ask.

## Model

`CheckpointRace.tla` models saves `A = {a1, a2}` and `B = {b1}`, each file two blocks, a game writing two saves of A, and a play-time writer holding B open for the whole session. Write calls are non-atomic. `Style` selects closing the handle after each call or holding it for the session; `Check` selects no check, length and mtime, a seqlock, write calls plus generation, or strict open-handle gating; `Scope` is per save or per volume; `PluginHint` models a correct plugin gate asked once before staging. Style `installs` models a write-back that replaces each file atomically with nothing marking the pass; style `pass` holds the pass as one writer. `ExpectComplete` names the saves whose writer bounds its saves.

Invariants: `FilesComplete` (every committed file existed with no write call in progress) `SavesComplete` (every committed save existed between guest saves), and `BoundedSavesComplete` (the same, for saves in `ExpectComplete` or with a plugin gate). Property: `CheckpointsResume` (once a save's writers stop, its latest state is committed).

| Configuration | Result | Distinct states |
|---|---|---:|
| `MCCheckpointRace.cfg` (write calls + generation, per save, handles held) | `FilesComplete`, `CheckpointsResume` pass | 281,111 |
| `MCCheckpointRaceClose.cfg` (handles closed per call) | pass | 281,111 |
| `MCCheckpointRaceHint.cfg` (plugin gate) | `FilesComplete`, `SavesComplete`, `CheckpointsResume` pass | 65,574 |
| `MCCheckpointRaceWritebackPass.cfg` (write-back held as one pass) | `FilesComplete`, `BoundedSavesComplete`, `CheckpointsResume` pass | 103,070 |
| `MCCheckpointRaceWritebackInstalls.cfg` (atomic files, no pass) | `BoundedSavesComplete` violated: a checkpoint between two installs commits half a save | — |
| `MCCheckpointRaceNoHint.cfg` | `SavesComplete` violated: a pause between files is committed | — |
| `MCCheckpointRaceHandlesHold.cfg` (strict handles, game holds its save) | `CheckpointsResume` violated | — |
| `MCCheckpointRaceHandlesVolume.cfg` (strict handles, per volume) | `CheckpointsResume` violated | — |
| `MCCheckpointRaceHandles.cfg` (strict handles, per save) | `CheckpointsResume` violated for the held save | — |
| `MCCheckpointRaceMtime.cfg` | `FilesComplete` violated | — |
| `MCCheckpointRaceNone.cfg` | `FilesComplete` violated | — |
| `MCCheckpointRaceSeqlockVolume.cfg` | `FilesComplete` violated: two writers leave the counter even | — |

Without a plugin gate, a committed save can be a state the guest paused in between files; this is the accepted risk of mid-session checkpoints. The model checks per-file activity, which the Rust implements per path and sums over a save's root. The gate's request contents, its timeout, and asynchronous plugin delivery are not modeled.

## Emulator survey

Source review on 2026-10-09 found that Dolphin, Azahar, and RPCS3 open save files only while the guest has them open; melonDS and ares write with short open–write–close bursts; mGBA keeps its `.sav` open and memory-mapped for the session; Cemu keeps `usr/save/system/pdm/` open for the whole game. Per-save scoping and the activity check allow mid-session checkpoints for all of them. Emulator-specific readiness belongs in plugin `onSaveCheckpoint` callbacks. The Cemu plugin defers a save whose `user/` files changed until Cemu has rewritten that save's `meta/saveinfo.xml`, which it does when the game flushes its save quota; the pending-change list has no order, so a commit followed by further writes before the next checkpoint is still accepted, and a game that never flushes is only checkpointed at exit. The RPCS3 plugin defers while any path in the request is under a `.working_*` or `.backup_*` directory, which RPCS3 uses to publish a save by renaming. While the save directory is briefly missing between those renames, the missing save's request carries every pending change on the volume, including the staging directories. Storage no longer special-cases dot-prefixed directories on PS3 or Vita; no Vita emulator plugin exists to take over that rule.

One public integration test in `tests/checkpoint_gate.rs` passes: with two changed PS1 saves and a gate rejecting one, the gate receives each save's root and changes, only the accepted save gains history, and a reopen after the simulated crash keeps the accepted save while rolling the rejected one back. Concurrent in-progress writes are covered by the model, not by a test.
