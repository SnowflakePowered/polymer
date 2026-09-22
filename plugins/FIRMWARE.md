# Firmware API

Firmware storage holds user-supplied BIOS files identified by consolespec. System updates use their own installation flow. Keys belong to the separate keystore described in [KEYS.md](KEYS.md).

## Recognition and installation

On drag-and-drop, Gumshoe runs first. If identification rejects a regular file, the host compares its complete-file MD5 and SHA-256 hashes with consolespec BIOS definitions. The first matching BIOS supplies the platform and all known names; its first name is the display label. Each input installs exactly one description. Filenames do not affect recognition.

The host retains an opened file handle through recognition and installation. On Windows it permits shared reads but denies writes and replacement during import. Only recognized BIOS files are streamed into a temporary destination and atomically published, using bounded memory. Recognition computes and caches only the digests it needs using streaming reads. Known consolespec hashes are stored only in the lookup index. Enumeration contains no hashes. To calculate a digest, open the file and call `file.digest(algorithm)`; the existing pluginhost `Digestible` implementation reads its actual stored bytes on demand. BIOS bytes are never sent to plugins for recognition or to scrapers.

## Reading and mounting

`onLaunch` receives `args.firmwareStore`; `onBootstrap(device, context)` receives `context.firmwareStore`. The host scopes each store to the launch console or bootstrap device platform.

```ts
const file = await args.firmwareStore.open("scph1001.bin");
// Equivalent:
const named = await args.firmwareStore.open({ name: "scph1001.bin" });
const exact = await args.firmwareStore.open({ sha256: expectedSha256 });
args.filesystem.mount("/system/bios.bin", file, { mode: "r" });
```

`open` accepts a name string, `{ name }`, or an object with `md5`, `sha1`, and/or `sha256`. With multiple hashes, only the first property is used; failure does not try later hashes. Names are exact and case-sensitive. Hash hexadecimal is case-insensitive. Empty lookups, missing matches, and ambiguous matches reject. Use a unique hash to disambiguate a name.

`list()` returns one description per installed BIOS, containing `platform`, `label`, `names`, and `byteLength`. Aliases do not duplicate enumeration entries. An empty store returns `[]`.

Opened files are read-only `FileEntry` resources named with the display label, supporting existing byte, text, and digest methods. Opened and mounted files retain their original content across replacement; later opens see the replacement.

## Storage

The library allocates `<storage>/firmware/firmware.db` and immutable blobs at `<storage>/firmware/<platform>/objects/<blake3>`. Metadata uses `(platform, label)` as its primary key. Separate `firmware_names` and `firmware_hashes` tables provide indexed, platform-scoped lookups. Import updates metadata, ordered names, and hashes in one transaction after staging content. Reimport replaces that slot. Superseded blobs are retained. This unshipped schema has no migration.

## Frontend import

`Polymer.library.import({ path })` returns `{ kind: "games", imports }` with launchable/scrape-job records or `{ kind: "firmware", firmware }` with one installed BIOS. `launchables.add` remains game-only. Horizon reports firmware imports through its activity UI.

Emulator BIOS-setting conversions and a firmware management screen are separate work.
