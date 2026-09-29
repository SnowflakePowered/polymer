# Key store

The library keeps opaque key records under `<storage>/keys`. Each record is identified by `(platform, namespace, id)` and may have one `titleId`. The host binds the platform. The store does not parse tickets or key files.

Platform handles accept consolespec machine IDs. Switch support is deferred.

Emulator `onLaunch` arguments expose a read-only `keyStore`. Platform `onBootstrap` context exposes a writer. Both support `list({ namespace?, titleId? })` and `open({ namespace, id })`; the writer also supports `put([{ namespace, id, titleId?, bytes }])` and `remove({ namespace, id })`. `open` returns a read-only `FileEntry`. JS code may persist already-decoded records, but ticket decoding is available only in the Rust console codecs.

Record conventions:

| Platform | Namespace | ID | Bytes |
| --- | --- | --- | --- |
| Wii, 3DS, Wii U | `content` | 16-digit native title ID | Decrypted 16-byte title key |
| PS3 | `content` | Full content ID | Original 16-byte RAP |
| Wii U | `content` | BLAKE3 digest of bytes | 16-byte disc key used by Cemu `keys.txt` |
| 3DS | `system` | 16-digit uppercase title ID | 16-byte NCCH seed from `seeddb.bin` |

Wii, 3DS, and Wii U ticket codecs and the PS3 RAP codec live in `polymer-consolestorage/impls/`. Concrete depots extract embedded keys, and the pending installation exposes them to the host. Hosts persist the keys before polling installation progress, which starts validation and extraction. Virtual NAND composition reads keys and generates ticket or RAP files in a read-only layer for each launch. Existing mounts do not refresh if the store changes during emulation.

External keys use `Polymer.library.importKey(platform, namespace, key, titleid?)`. `namespace` is `"system"` or `"content"`; `key` is a `Uint8Array` or an even-length hexadecimal string. JavaScript converts hexadecimal strings to bytes before calling Rust. The API accepts decoded key material, so callers parse key files themselves. Package depots extract embedded tickets in Rust.

For system keys, `titleid` is the native key name; if omitted, the bytes' digest becomes the ID. For content keys, `titleid` is stored as the record ID and title association. Callers supply the native title or content ID needed by the consumer. Omitting it stores an unassociated record under the bytes' digest, which Cemu can use as a disc-key candidate. The Cemu plugin emits `keys.txt` from those candidates.

IDs are exact and case-sensitive. Wii and Wii U codecs emit lowercase 16-digit title IDs; 3DS emits uppercase. PS3 content IDs use the canonical filename spelling, and RAP bytes retain their original representation.

Batches replace only their supplied identities, including the optional title association. Duplicate identities reject the entire batch. Enumeration combines namespace and title filters, orders by namespace then ID, and never includes key bytes. Open files and composed NANDs keep their original bytes after replacement or removal.

Importing a `seeddb.bin` through `Polymer.library.import` stores its seeds for both `NINTENDO_3DS` and `NINTENDO_N3DS` and returns `{ kind: "keys", platform, count }`. CIA installation reads the seed for seed-encrypted NCCH contents, checks it against the NCCH seed check, and decrypts with it; the install fails if the seed is missing or wrong.

Personalized Nintendo tickets currently fail preparation with an explicit unsupported depersonalization error; they are never imported as ordinary common tickets. Console/device cryptographic material is not exposed through JS ticket parsers. Generated licenses use placeholder signatures and are intended for launch, not guest-side installation.
