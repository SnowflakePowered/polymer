# Formal Verification Candidates

Candidates for formal specification and proof, with the strategy for each.

- **Kani**: bounded model checking of small pure-Rust kernels (codecs, path logic, on-disk formats, arithmetic).
- **TLA+**: cross-process protocols, crash consistency, and concurrency, where bugs are in interleavings rather than code.
- **loom / proptest**: noted where they fit better than either.

Status: none implemented. No `kani`, `proptest`, or `loom` dependencies exist in the workspace yet.

## Kani

| # | Target | Location | Properties |
|---|--------|----------|------------|
| K1 | Input wire codec | `crates/runtime/polymer-ipc/src/input/mod.rs` (`WireInputMessage::encode`/`decode`) | `decode(encode(m)) == m` (bitwise for f32); decode never panics on any buffer. **Known lossy fields:** `HostPointer.button` (`u32` encoded `as u16`), `HostKey.code` (via `i16`), `phase` (`as u8`). |
| K2 | Control-channel wire codec | `crates/runtime/polymer-ipc/src/control/wire.rs` | Encode/decode round-trip; decode of arbitrary `WireMessage` + data never panics or over-reads (`payload<T>`, `data_len`). Carries handles across processes. |
| K3 | VFS path normalization and mount resolution | `crates/filesystem/polymer-vfs/path/` (`components.rs`, `vfs.rs`), `plan.rs::resolve` | `Components` forward == reversed backward, no duplicate yields; normalization never yields `..` or escapes root; `resolve` picks the longest matching binding with no `/foo` vs `/foobar` prefix confusion; validation agrees across `u8`/`u16` paths. Security boundary (jail). |
| K4 | `polymer-config` emitters: injection and round-trip | `crates/polymer-config/src/ser/emitter/{ini,xml,bml}.rs`, `mod.rs` (`EmittedValue::render`) | `parse(emit(doc)) == doc`; emitted output contains exactly the instance's keys. **Suspected bug:** `String`/`Path` values are emitted verbatim with no newline/control-char validation found in `polymer-config`, so `\n[Section]\nkey=value` can inject INI content. Confirm no upstream layer rejects it. BML is the most likely to be wrong. |
| K5 | `polymer-config` arena and override handling | `crates/polymer-config/src/arena/non_max_u32.rs`, `handle.rs`; override application | `NonMaxU32` round-trip and sentinel never produced; "never destroy user intent": applying overrides and re-extracting preserves unknown keys and orphaned values, and is idempotent. |
| K6 | HCI ACL credits | `crates/input/polymer-bluetooth/src/transport/bredr/hci/credits.rs` | Credit conservation across `try_acquire`/`complete`/`forget_handle`; `outstanding()` sum cannot overflow; `handle & 0x0fff` masking applied consistently. |
| K7 | Memory card formats | `crates/filesystem/polymer-consolestorage/impls/ps2/raw/card/*`, `gamecube/raw/card.rs`, `ps1/raw/card.rs`, `memorycard/raw.rs` | Superblock/directory/FAT parsers never panic; FAT chain walks terminate on cyclic chains; geometry arithmetic doesn't overflow; per-structure `parse(write(x)) == x`. Whole-card round-trips go to proptest. |
| K8 | Installable and container parsers | `consolestorage/impls/ctr/depots/cia/ncch.rs`, PS3 PKG, Wii/Wii U NAND, `ps2/psu.rs`, `keys.rs` | No panics; no overflow in offset/size arithmetic; every computed range stays in bounds. Symbolic header plus offset table rather than whole files. |
| K9 | Swapchain arithmetic kernels | `crates/producers/polymer-presentation/src/swapchain/{budget,pacing,latency}.rs`, `state.rs` (`ImageSlot::new`) | No overflow near `u64::MAX` timeline values; monotonic pacing target; slot bounds respect `MAX_IMAGE_SLOTS`. Complements T1. |
| K10 | Controller topology, mapping, and hub assignment | `crates/runtime/polymer-controllers/src/topology/`, `mapping.rs`, `topology/wire.rs`; `crates/input/polymer-input/src/hub/assignment.rs` | No two devices share a slot; stable reassignment on reconnect; every slot maps to a valid consolespec binding key; topology wire round-trip. |
| K11 | L2CAP packet parsing | `crates/input/polymer-bluetooth/src/transport/bredr/l2cap/{packet,signaling}.rs` | No panics on hostile radio input. Lower priority. |
| K12 | Bluetooth HID report decoders | `crates/input/polymer-input-bluetooth/src/decoder/` (wiimote, switch, switch2, dualshock3/4, dualsense, sony) | No panics for any report length or contents; decoded axes and triggers stay in range. `wiimote.rs` mixes `.get(..)` with direct indexing (`data[..5]`, `data[5]`, `data[index * 9..]`), so some paths rely on unstated length invariants. Input arrives from any device in radio range. |
| K13 | Exclusive resource path overlap | `crates/filesystem/polymer-vfs/resource/exclusive.rs` (`normalize_host_path`, `path_keys_overlap`) | Overlap is symmetric and holds iff one path is an ancestor of, or equal to, the other. **Suspected gaps:** a root with a trailing separator (`c:\` vs `c:\foo`) doesn't overlap because the suffix doesn't start with `\`; no case folding on macOS even though APFS is case-insensitive by default; `/` → `\` replacement on macOS conflates names containing `\`; `to_lowercase` differs from NTFS's upcase table. |
| K14 | Plugin `pkg:` path resolution | `crates/runtime/polymer-pluginhost/filesystem/mod.rs` (`package_path`) | The result stays under `package_root` for every input string (percent-decoding, separators, prefixes). Check Windows edge cases such as `ab:c` (NTFS alternate data stream as a `Normal` component). Sandbox boundary for plugins. |
| K15 | Bluetooth binary formats | `crates/input/polymer-bluetooth/src/transport/bredr/sdp/`, `firmware/realtek.rs`, `store.rs` | SDP requests from peers, Realtek firmware patch parsing, and persisted link-key store parsing never panic, never overflow offsets, and stay in bounds. |
| K16 | Disc serial normalization | `crates/runtime/polymer-library/src/games/discs/serial.rs` (`normalize`) | `normalize` is idempotent, and two serials that should group into one multi-disc set normalize to the same key. `LaunchableUri` `FromStr`/`Display` round-trip (`games/uri.rs`) fits proptest better, since it goes through `url`. |
| K17 | CUE template extraction | `crates/runtime/polymer-scraping/src/ingest/cue.rs` (`CuePreparation::finalize`, `operand`) | Rendering the template with the original file names reproduces the original CUE text byte for byte; every `FILE`/`CDTEXTFILE` operand becomes exactly one `CuePart::File`; slicing never panics (BOM, quoted vs unquoted, CRLF, case variants). |
| K18 | FSKit request framing | `crates/filesystem/polymer-vfs-fskit/src/wire.rs`, `server.rs` | Frames over `MAX_FRAME` and reads over `MAX_IO` are rejected before allocation; handle lookups after `Close` fail rather than alias a new handle. The serde codec itself fits proptest better than Kani. |
| K19 | In-memory storage file | `crates/filesystem/polymer-consolestorage/storage/memory/file.rs` | `write_at` then `read_at` returns the written bytes; gaps zero-fill; `set_len` truncates and extends correctly; offset arithmetic never overflows. Backs `Configuration` and `Uint8Array` mounts. |

## TLA+

| # | Target | Location | Properties |
|---|--------|----------|------------|
| T1 | Swapchain presentation protocol | `crates/producers/polymer-presentation/src/swapchain/` (`controller.rs`, `retirement.rs`, `state.rs`), `crates/consumer/polymer-consumer-d3d12/src/timeline.rs` | Producer, consumer, and controller as processes, with crash/disconnect/device loss as nondeterministic steps. **Safety:** no producer write while the consumer may read (published > released ⇒ no reacquire); memory never freed with unreleased publications; stale-generation leases can't submit; monotonic `consumer_release` and timeline values; `CounterExhausted` behavior. **Liveness:** `acquire_paced` eventually succeeds under fair release; retirement reaches `Safe` or `Quarantined`. 2–3 slots, 2 generations. |
| T2 | Surface matching | `crates/producers/polymer-presentation/src/swapchain/matching.rs` | A binding never attaches to a recycled window handle or re-created device object (ABA on `SourceIdentity`/`WindowIdentity` lifetimes). Can be folded into T1. |
| T3 | Save-history materialization journal | `crates/filesystem/polymer-consolestorage/history/transaction.rs` | Crash possible between every ref edit and filesystem write. After recovery the depot is at exactly `old` or `target`; attachments never point at another tip; recovery is idempotent; `RollbackFailed` handling. |
| T4 | Reconcile and git-backed save storage | `crates/filesystem/polymer-consolestorage/storage/reconcile/`, `storage/git/` | Concurrent guest writes vs. reconcile worker vs. emulator exit: every write acknowledged to the guest ends up in a commit or the scratch area. Install-as-commit and uninstall-as-revert commute with save commits (separate trees). |
| T5 | Scraper dependency scheduling | `crates/runtime/polymer-scraping/src/engine/` | No deadlock on cyclic `provides`/`requires`/`any(...)`; every scraper whose requirements become satisfiable eventually runs exactly once. |
| T6 | XPC broker and connection lifecycle | `crates/runtime/polymer-ipc/src/xpc/{broker,connection,queue}.rs` | No lost wakeups; `close` unblocks all receivers; full queue (cap 64) cannot deadlock the call primitive when request and reply share a queue. `queue.rs` alone could use `loom`. |
| T7 | Transport handshake end to end | `crates/runtime/polymer-ipc/src/control/`, `shared.rs` (`Publisher`/`Subscriber`), consumer timeline open | Peer crash at any step; duplicate offers for the same stream; `DuplicateHandle`d handles never leak or end up double-owned. |
| T8 | Library job runner and import | `crates/runtime/polymer-runtime/src/jobs/`, `crates/runtime/polymer-library/src/jobs/` | Cancellation vs. completion vs. re-enqueue of the same game; no permanently pending jobs. |
| T9 | L2CAP configuration negotiation | `crates/input/polymer-bluetooth/src/transport/bredr/l2cap/signaling.rs`, `link.rs` | Negotiation terminates and agrees on both sides. Low priority; real devices are the practical oracle. |
| T10 | Bluetooth manager sessions and pairing | `crates/input/polymer-bluetooth/src/manager/` (`sessions.rs`, `discovery.rs`, `connection/`), `transport/bredr/pairing.rs` | Connect, disconnect, reconnect, and pairing races across the two radios (BLE onboard, EDR WinUSB). At most one session per device address; a link key is persisted before the device is reported paired; a device that disconnects mid-pairing doesn't leave a stuck session. |
| T11 | VFS resource leases across launches | `crates/filesystem/polymer-vfs/resource/` (`lease.rs`, `exclusive.rs`) | Two launches never hold overlapping resources; every acquired lease is finalized exactly once across `LaunchFailed`/`ProcessExited`/`Cancelled`; partial acquisition in `CompositeResourceLease::exclusive` releases what it took. Pairs with K13. |
| T12 | Composed storage cross-layer rename | `crates/filesystem/polymer-consolestorage/storage/composed.rs` (`rename`, `move_binding`, `ComposedFile::redirected`) | Model layers, open handles, and renames, with crashes between steps. **Windows to check:** a write through an open handle to `from` after its bytes are copied but before `move_binding` would be lost; a same-layer directory rename doesn't update `redirected_to` for files beneath it that were redirected earlier; a crash after the staged rename but before `remove(from)` leaves both `from` and `to` visible in the union listing. Property: every acknowledged write is visible at the file's current path. |
| T13 | Plugin download resume | `crates/runtime/polymer-pluginhost/download/transfer.rs` | Model crashes, server content changes, and 200 vs 206 responses. Property: a completed download equals a single server version. **Gaps to check:** `Content-Range` on a 206 isn't validated against the requested offset; downloads without size or digest (`checkable() == false`) and without ETag/Last-Modified can splice two versions undetected; `File::flush` isn't durable, so a checkpoint can get ahead of the bytes on disk after power loss. |

## Other tools

| Target | Location | Tool |
|--------|----------|------|
| `bcast` shared-memory ring (reader reset on overrun) | `crates/runtime/polymer-ipc/src/shared.rs` | `loom` or a short TLA+ model; third-party crate, so formal proof isn't worth it |
| Input calibration | `crates/input/polymer-input/src/calibration.rs` | proptest (float-heavy) |
| Compositor clock | `crates/compositor/polymer-composition-windows/src/window/clock.rs` | proptest (float-heavy) |
| Whole memory card round-trips | see K7 | proptest |
| Vulkan layer acquire wakeup | `crates/hooks/polymer-hook-vulkan-layer/src/swapchain/wakeup.rs` | `loom`; reads correct (epoch bumped under the lock), so this is cheap confirmation only |
| Pointer projection | `crates/consumer/polymer-renderer/src/pointer/mod.rs` | proptest (float-heavy; zero extents and NaN already guarded) |

## Not worth verifying

Hook and vtable crates, D3D/Vulkan/Metal FFI, the Deno runtime, the GUI, `polymer-statistics`, `polymer-banner`. Their correctness depends on external API behavior that would have to be axiomatized.

## Suggested order

1. **K4**: likely a real INI injection bug; confirm first.
2. **K1**: about an hour; flags the known lossy fields.
3. **T3**: protects user saves.
4. **T1**: the most likely to find rare hangs or torn frames.
5. **K6**, **K2**: about an hour each.
6. **K3**: security boundary.
7. **K13**: confirm the root and macOS case-folding gaps.
8. **K12**, **K14**: hostile input and plugin sandbox.
9. **T12**: save data written through composed storage.
10. **K17**, **T13**.

## Practical notes

- Kani harnesses live under `#[cfg(kani)]`, which conflicts with AGENTS.md's "no inline tests" rule. Decide between a separate verification crate and in-crate harnesses, given that some targets are `pub(crate)`.
- TLA+ specs would sit beside their crate, e.g. `crates/producers/polymer-presentation/spec/Swapchain.tla`.
