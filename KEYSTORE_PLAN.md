# Keystore implementation plan

## 1. Agreed behavior

The library owns a keystore containing opaque byte records. Both Rust and JS can enumerate, read, write, and remove records. Each handle is bound to one consolespec platform by the host.

The store has no parsers, ticket codecs, import/export formats, NAND paths, or ticket-generation logic. Records contain only `namespace`, `id`, optional `titleId`, and bytes. `namespace` is `system` or `content`. There is no generic metadata field or array of title associations.

Console codecs live under `polymer-consolestorage/impls/`. They extract keys from tickets and generate structurally correct tickets with placeholder signatures. A concrete `VirtualNand` reads the keystore during composition and creates its own ticket or license files. Emulator plugins create separate emulator key files from the same store.

Concrete depots extract embedded keys. The existing pending `Installation` exposes those records so the host can persist them **before polling the installation**. NAND validation and extraction remain deferred until polling. An imported key remains available if subsequent validation, extraction, or completion fails or is cancelled.

`AppTicket` is removed. There is no installed data requiring migration, so the schema can change directly. Guest-side installation is outside the supported workflow.

## 2. Record model and persistence

The unique identity is `(platform, namespace, id)`. The host binds the platform; callers supply the other fields. `titleId` is a nullable, indexed association. Several keys may be associated with one title, but each record has at most one associated title. It has no foreign key into the game catalog: keys can precede their games and survive game deletion.

Descriptions expose identity, optional title ID, and byte length. Enumeration does not return key bytes or internal blob digests.

Use the firmware store's persistence pattern:

```text
<storage>/keys/keys.db
<storage>/keys/<platform>/objects/<blake3>
```

Add `LibraryDirectories::keys()`. Store metadata in a dedicated Turso database with one records table and an index on `(platform, title_id)`. A table row contains platform, namespace, id, nullable title_id, blob digest, and byte length. No separate association table is needed.

Stage immutable blobs before publishing their metadata in a transaction. `put` accepts a batch and atomically inserts or replaces those records. Replacement also replaces the title association. Reject duplicate identities within a batch so ordering cannot silently decide the result. Identical reimports have the same observable result. Do not remove records omitted from the batch.

Names and IDs are exact, case-sensitive lookup strings. Codecs canonicalize native identifiers before writing. The store checks structural invariants and safe platform/object paths, but does not validate cryptographic key lengths. Arbitrary record IDs never become host filesystem paths.

Opened resources continue to reference their original immutable objects after replacement or removal. Retain superseded blobs initially, as firmware storage does; garbage collection is separate work. No live key refresh is required for mounted NANDs.

## 3. Rust API

Define the shared record types, errors, and object-safe reader/writer traits in `polymer-consolestorage/keys.rs`, or `keys/mod.rs` if it needs multiple files. Implement persistence in `polymer-library/src/keys/mod.rs` with separate read and write modules as needed. This preserves the existing dependency direction from library to console storage.

The following is the intended API shape; normal derives and serialization attributes are omitted:

```rust
pub struct KeyIdentity {
    pub namespace: String,
    pub id: String,
}

pub struct KeyRecord {
    pub identity: KeyIdentity,
    pub title_id: Option<String>,
    pub bytes: Vec<u8>,
}

pub struct KeyDescription {
    pub identity: KeyIdentity,
    pub title_id: Option<String>,
    pub byte_length: u64,
}

#[derive(Default)]
pub struct KeyQuery {
    pub namespace: Option<String>,
    pub title_id: Option<String>,
}

#[async_trait]
pub trait KeyReader: Send + Sync {
    async fn list(&self, query: &KeyQuery)
        -> Result<Vec<KeyDescription>, KeyError>;

    async fn read(&self, identity: &KeyIdentity)
        -> Result<KeyRecord, KeyError>;
}

#[async_trait]
pub trait KeyWriter: KeyReader {
    async fn put(&self, records: &[KeyRecord]) -> Result<(), KeyError>;
    async fn remove(&self, identity: &KeyIdentity) -> Result<bool, KeyError>;
}
```

`KeyStore::open(root).await` opens persistence. `store.for_platform(platform)` produces a validated, platform-bound handle implementing these traits. No new store or database is created for each platform handle. Expose typed errors for missing records, invalid requests, I/O, and persistence failures; format/decryption errors belong to codecs.

`list` combines supplied filters with AND, uses deterministic ordering, and includes unassociated keys when no title filter is supplied. Exact reads reject missing records. `remove` reports whether the record existed.

The library may expose an immutable-file resolver for pluginhost, as firmware does. A Rust consumer reads bytes through `KeyReader`; pluginhost uses the resolver to create a read-only `FileEntry`. This is resource adaptation, not another record representation.

## 4. JS API and host bindings

```ts
interface KeyIdentity {
  namespace: "system" | "content";
  id: string;
}

interface KeyInput extends KeyIdentity {
  titleId?: string;
  bytes: Uint8Array;
}

interface KeyDescription extends KeyIdentity {
  readonly titleId?: string;
  readonly byteLength: number;
}

interface KeyQuery {
  namespace?: "system" | "content";
  titleId?: string;
}

interface KeyReader {
  list(query?: KeyQuery): Promise<readonly KeyDescription[]>;
  open(identity: KeyIdentity): Promise<FileEntry>;
}

interface KeyWriter extends KeyReader {
  put(records: readonly KeyInput[]): Promise<void>;
  remove(identity: KeyIdentity): Promise<boolean>;
}
```

Add `keyStore` to launch and bootstrap contexts, following the firmware binding pattern. Launch receives a reader. Host-owned import/setup contexts receive a writer. Enforce the bound platform and available operations in Rust, not just TypeScript declarations. Reuse existing FileEntry byte methods and read-only resource enforcement.

Implement the binding under `polymer-pluginhost/keys/` with `mod.rs`, Rust ops, and the JS facade. Update worker initialization, launch/bootstrap context injection, and `plugins/plugin-api.d.ts`. Generalize the existing read-only file-resource origin handling so key resources are not incorrectly labeled as firmware.

JS callers that already have key records call `put` directly. The application exposes `Polymer.library.importKey(platform, namespace, key, titleid?)`; JavaScript converts hexadecimal strings into bytes before calling Rust. JS has no ticket-decoding API. Embedded package tickets are decoded by Rust host code, which then writes their records to the keystore.

## 5. Console codecs and byte conventions

Place applicable codecs beside the NAND implementations:

```text
polymer-consolestorage/impls/
  wii/ticket.rs
  ctr/ticket.rs
  wiiu/ticket.rs
  ps3/rap.rs
```

Move the existing Wii parser out of `impls/wii/depots/wad/ticket.rs` so package extraction, WAD processing, and NAND composition share it. Reuse existing CIA parsing support where it already exposes ticket sections; avoid a second independent parser for the same structure. Extract the PS3 content-ID and RAP validation from its depot implementation.

Use struct implementations and zerocopy for binary layouts where practical. Share a trait only for behavior genuinely common to multiple codecs. Console-specific fields and ticket construction stay inside each console module.

Document fixed byte conventions per platform. Initial conventions:

| Platform/material | Namespace | ID | Bytes |
| --- | --- | --- | --- |
| Wii/3DS/Wii U title key | `content` | Canonical native title ID | Decrypted 16-byte title key |
| PS3 RAP | `content` | Full canonical content ID | Original 16-byte RAP representation |
| Switch title key | `content` | Full rights ID | Common-ticket/title.keys wrapped 16-byte representation |
| Named system key | `system` | Native name, including revision where present | Native key bytes; length is name-specific |
| Wii U disc candidate | `content` | Digest of the key bytes | 16-byte disc key |

The platform-bound type determines the representation. Do not silently interchange plaintext and wrapped title keys. Codecs handle any conversion, using separately stored system keys or existing console crypto support as appropriate. RAP material is not a plaintext PS3 decryption key.

When decoding requires missing system/device material, return an explicit dependency error before installation construction. Do not add an opaque metadata bag or a pending-ticket database to accommodate that case. Personalized tickets require actual depersonalization before their personal fields can be discarded.

Generated Wii/3DS/Wii U tickets use the stored key and title identity. Codecs synthesize stable ticket IDs, appropriate headers, non-personalized device/account fields, and permissive content-rights structures. Use installed title information for fields such as title version where needed. Correctly encrypt the title-key field for the emitted ticket; a placeholder signature does not replace that encryption.

3DS rights generation must cover the required content indices, including indices above 255. Wii U generation must produce the DLC rights structure Cemu reads. Original ticket signatures and purchaser identity are not required for the supported launch paths.

## 6. Extract and persist before driving installation

Keep all existing installation interfaces free of keystore arguments. The host owns this ordering:

```rust
let installation = nand.try_install(&path)?;
let records = installation.embedded_keys();
if !records.is_empty() {
    let store = KeyStore::open(key_root).await?;
    store.for_platform(platform)?.put(records).await?;
}

// Persistence has completed before NAND validation or content writes begin.
let artifact = installation.finish().await?;
```

Each concrete depot extracts embedded records through `Depot::embedded_keys`. `Installation::new` collects them before returning the pending installation. Ticket parsing remains in console codecs. A package without a ticket yields an empty batch. An identified but malformed ticket is an error, not silently treated as absent. NAND compatibility and content validation occur when the installation is first polled, after key persistence.

Apply this preparation to every host entry point that constructs an installation:

- `polymer-runtime/src/consolestorage/operations.rs` frontend installation.
- `polymer-pluginhost/platform/bindings.rs` bootstrap `device.install`.
- `polymer-pluginhost/platform/source.rs` shared installation entry point.

Preserve the entry points' existing leases, progress reporting, source resolution, and cancellation behavior. Finish key persistence before polling installation progress or completion.

For a JS-controlled installation, host preparation handles embedded tickets in packages passed to `device.install`. JS can use `keyStore.put` only when it already has key records. Repeated persistence is safe.

The concrete content installer parses and decrypts its package using its existing format implementation. It does not look up or write the keystore. `nand_depots!` retains concrete depot dispatch, and the existing lazy installation preparation validates the NAND and contents after key persistence.

If preparation succeeds but subsequent installation fails, the key remains. There is no transaction spanning the keystore and NAND history, and no key rollback tied to game installation or removal.

## 7. Remove ticket depots and persistent ticket files

Remove `DepotType::AppTicket`, its serialization variants, dispatch branches, and frontend/plugin declarations. Change the unshipped schema directly; do not add migrations or compatibility readers for that variant.

Specific changes:

- Remove `CiaDepot::ticket_depot()` and its ticket-only branches. CIA package preparation extracts the embedded key; the host persists it before driving the ordinary CIA installation.
- Remove CIA manifest retention of ticket files and the ticket-only artifact fallback in installed-title metadata.
- Remove `Ps3RapDepot` as a NAND depot. External callers can persist RAP bytes as a `content` record without requiring a NAND or installed system software.
- Stop the WAD installer from writing `.tik` files or including them in its artifact manifest. Preserve its TMD and UID-map work.
- For supported Wii U directory sources containing `code/title.tik`, extract it during host preparation and exclude it from persistent content installation. Generate that path during composition instead.
- Update exhaustive depot-type matches, display mappings, and existing integration fixtures affected by these behavior changes.

Preserve package content validation, including installed-content hashes. Removing a ticket artifact does not remove the need to decrypt or validate package content.

## 8. NAND composition

Make `VirtualNand::compose` asynchronous and pass the platform-bound reader directly:

```rust
async fn compose(
    &self,
    saves: Option<Arc<dyn SaveRepo>>,
    keys: &dyn KeyReader,
) -> Result<Arc<dyn MountableStorage>, Error>;
```

Do not introduce `KeySnapshot`. Each concrete NAND enumerates relevant records, reads their bytes, and uses its codec to generate files. The returned storage owns those files for the session and does not consult the store during emulation. NANDs without applicable key formats simply compose their existing storage and optional saves.

Create a reusable immutable generated-file storage layer implementing the existing storage interfaces. It provides accurate metadata, directory enumeration, reads, and denied writes. Console implementations supply the generated file paths and bytes; the layer does not know ticket formats.

Compose generated license files, saves, and the system NAND using `ComposedStorage`. Make path ownership explicit: generated files take precedence at their exact destinations, while parent-directory handling must preserve unrelated NAND/save entries. Check both `claims()` behavior and directory enumeration, since first claimant wins even when its file is absent.

Initial destinations:

- Wii: `ticket/<title-high>/<title-low>.tik`.
- 3DS: the existing Azahar-compatible `nand/dbs/ticket.db/<title-id>.<ticket-id>.tik` layout.
- Wii U: the installed title's `code/title.tik` path consumed by Cemu.
- PS3: `dev_hdd0/home/00000001/exdata/<content-id>.rap`.

Apply existing console permission policies so guest writes cannot enter generated key files or save history. Synthesis may use installed title information; selection belongs to the concrete NAND and must account for installed titles/DLC rather than assuming only the foreground game's title ID matters.

Update `polymer-runtime/src/storage.rs` so both save-plus-NAND and NAND-only selections call `compose(...).await`. The current NAND-only branch mounts `nand.root()` directly and must be changed. Existing plugins continue to mount the resulting resource through `args.storage`.

## 9. External keys and emulator key files

Callers parse external `prod.keys`, `title.keys`, Cemu `keys.txt`, and other key sources before invoking `importKey`. Keep file parsing outside `KeyStore`. Binary ticket decoding remains in Rust console codecs and is not exposed to JS.

External keys enter through `Polymer.library.importKey(platform, namespace, key, titleid?)`, separately from the regular library import path. `namespace` is `"system"` or `"content"`. JavaScript converts hexadecimal strings into `Uint8Array` before calling Rust. The API accepts raw key material and has no type or format argument. Callers parse external key files themselves; JS does not decode tickets. For system keys, `titleid` serves as the native key name when supplied.

Package installation takes the embedded-ticket preparation path even when Gumshoe recognizes the package as a game. A BIOS/game recognition fallback alone cannot cover embedded keys.

For launch-time files:

- `prod.keys`: enumerate named system records and emit deterministic `name = hex` entries.
- `title.keys`: enumerate Switch title-key records and emit `rights_id = hex` in the documented wrapped representation.
- Cemu `keys.txt`: enumerate unassociated `content` records and emit one 32-character hexadecimal key per line. Do not export all eShop title keys as disc keys.

Plugins mount the resulting `Uint8Array` through the existing filesystem API. The keystore has no `importFile` or `exportFile` operation. Switch key-file support does not require inventing a Switch NAND implementation in this change.

## 10. Verification and delivery sequence

Implement in this order:

1. Shared record types/traits, library persistence, and platform-bound handles.
2. JS reader/writer bindings and context injection.
3. Console ticket/RAP codecs and separate package-key preparation.
4. Host persistence before installation polling; removal of ticket depots and persistent ticket files.
5. Async NAND composition and generated read-only license layers, including NAND-only launches.
6. Separate raw key import API and relevant plugin key-file output.
7. Integration verification and documentation.

Use integration tests under `tests/`, with real temporary storage and public APIs. Do not add inline tests, mocks, API-shape tests, or TypeScript plugin tests.

Meaningful checks include:

- A failed batch does not publish a partial set of key records; replacement/removal does not change an already-open file resource.
- Rust and JS access the same persisted records through their public bindings, with platform isolation and optional title lookup.
- A package's key is available even when installation construction subsequently fails; a package without a ticket follows its ordinary install behavior.
- Ticket decode and generated-ticket decode recover the same key using dummy cryptographic material, including supported personalized handling and missing-dependency errors.
- Public composed storage exposes generated ticket files and their directories alongside existing saves/content, denies writes, and leaves NAND artifacts and save history untouched by generation.
- Both NAND-only and save-plus-NAND launch preparation provide generated keys.
- Known format fixtures verify text key-file output and significant ticket details such as 3DS content rights above index 255. Avoid tests that merely duplicate encoder structure.

Run formatting and compile checks for affected crates, plus the relevant existing integration suites. Where suitable local titles/emulators are available, validate installed-title launch and DLC visibility with generated tickets through `polymer-gui --debug`. Report source-level verification separately from actual emulator runs; do not claim unperformed launch tests.

Document the public contract in `plugins/KEYS.md` and update plugin API declarations and firmware documentation cross-references. Keep `ARCHITECTURE.md` human-written as instructed. Its existing three-plugin-class description omits the implemented platform plugin class; report that discrepancy rather than editing it.

## 11. Compatibility boundary from the source audit

The inspected Azahar, Cemu, and Switch emulator paths accept generated tickets without a genuine issuer signature. Eden already synthesizes tickets from stored title keys. RPCS3's RAP path has no ticket-signature requirement.

Dolphin's guest `ES_Launch` reaches its ordinary installed-title launch path, which checks ticket structure rather than authenticity. Its separate `ES_SetUpStreamKey` path performs genuine signature verification. Guest installation is excluded by requirement; stream-key compatibility remains a specific edge case to investigate if an affected game is encountered. Do not make original signed-ticket retention a mandatory part of this store.

Source references supporting the design:

- [Dolphin guest launch](https://github.com/dolphin-emu/dolphin/blob/ee018d00e60b9eb727489908a8daec5c537f44a8/Source/Core/Core/IOS/ES/ES.cpp#L753) and [stream-key verification](https://github.com/dolphin-emu/dolphin/blob/ee018d00e60b9eb727489908a8daec5c537f44a8/Source/Core/Core/IOS/ES/ES.cpp#L919).
- [Azahar ticket parsing and content rights](https://github.com/azahar-emu/azahar/blob/c2237de04d8c08cb5ad0ba3fb98e5a9640203257/src/core/file_sys/ticket.cpp).
- [Cemu ticket codec](https://github.com/cemu-project/Cemu/blob/3310f3b8b184d64a62b89fd59088c799432badf5/src/Cemu/ncrypto/ncrypto.cpp) and [DLC ticket path](https://github.com/cemu-project/Cemu/blob/3310f3b8b184d64a62b89fd59088c799432badf5/src/Cafe/OS/libs/nn_aoc/nn_aoc.cpp#L110).
- [Eden ticket synthesis](https://github.com/eden-emulator/mirror/blob/1203082a8f1211ad6ce4d95f0c6947f7ecd27a55/src/core/crypto/key_manager.cpp#L176).
- [RPCS3 RAP conversion](https://github.com/RPCS3/rpcs3/blob/6c0e2823f83037debd9a8ac8541521e61d7663bd/rpcs3/Crypto/unedat.cpp#L684).

Delivery is complete when the shared store is usable from Rust and JS, supported package keys persist before installation polling, applicable NANDs generate their license files from the store, non-NAND emulator key files can be produced, and AppTicket no longer participates in installation or storage.
