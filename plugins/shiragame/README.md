# Shiragame scraper

Adds exact cue-sheet bytes as `media.cue` when a game matches a shiragame serial
or dump hash within its platform. The plugin downloads the
[latest release](https://github.com/SnowflakePowered/shiragame/releases/latest)
and reads its release identifier from the database metadata. It does not match titles.

The plugin is discovered alongside the other bundled plugins through `plugin.toml`.
The first eligible scrape downloads the database archive and builds local lookup
indexes. This can take several minutes. Subsequent scrapes query the cache without
network access. The normal scraper refresh action downloads and indexes a new copy.

## Matching and media

- Serial lookup considers the original serial and the producer's platform-specific
  normalization. Distinct disc/revision matches remain separate media candidates.
- SHA-1 is preferred, then MD5, then CRC32 with byte size. A matched BIN track
  resolves to its game and retrieves the sibling CUE record.
- Additional digests and size from the same source must agree with the matched
  dump where the database supplies them. Conflicting known hashes do not fall back
  to serial matches. An unknown hash can still fall back to a matching serial.
- Fingerprints are grouped by metadata source so hashes from different files are
  never cross-paired. Ambiguous groups with multiple distinct values are skipped.
- Identical cue payloads are emitted once. Encoding, line endings, and canonical
  `FILE` names are preserved. Scraping stores candidates in the library; it does
  not rename local files, install a cue alongside a game, or change its launchable.

Media provenance includes the database release, cataloguing organization, game
entry name, and canonical cue filename. Shiragame IDs are used only within the
current database query, never as persistent cross-release identities.

The host accepts inline media in addition to existing HTTP(S) media URLs:

```ts
yield {
  key: "media.cue",
  value: {
    kind: "inline",
    mediaType: "application/x-cue",
    bytes: cue.contents, // number[] from a SQLite BLOB
  },
  source: "Shiragame / release / organization / entry / filename",
};
```

The runtime validates each result before retaining it. This plugin limits each
lookup to 128 matching dumps/games and each scrape to 256 distinct cues. Excessive
ambiguity fails the scrape rather than silently choosing or truncating matches.
Successful rescapes replace only this plugin's media; failures preserve the last
successful result, and an empty successful match clears stale shiragame candidates.

## Lazy file hashes

The host supplies packages and raw dump file objects through the scraper context.
Shiragame requests SHA-1, MD5, and CRC32 with `file.digest(algorithm)` and checks
the file's size. Each file stays distinct when root and child packages or CUE
backing files are combined. Import does not compute or persist these dump hashes.
Existing launchables can be scraped directly without reimporting for hashes.

Directories and recognized compressed/container formats are excluded from raw
dump matching; serial matching remains available. Header stripping and decryption
are not applied to file digests, so only byte-identical dump representations match.
`package.binary.digest()` is the separate normalized-executable interface.

## Cache lifecycle

The archive is streamed into the plugin's declared `download` cache and extracted
natively by `polymer:unarchive`, with ZIP CRC verification, into its generation's
directory. Database validation
checks schema compatibility, required columns, and SQLite integrity. The release
has no hash/serial lookup indexes, so the plugin builds them in its staged copy.

Only after indexing, checkpointing, and closing the staged database does one
SQLite statement publish its generation in the plugin's persistent database.
The host serializes `onUpdateContents` calls per plugin. Existing readers retain
an immutable generation, so a failed or cancelled update cannot replace the
active database. Older and failed staging files remain in the managed cache;
clearing that cache reclaims them and causes a rebuild on the next plugin load.
The release archive is capped at 256 MiB and the extracted database at 1 GiB.

`connect(caches.download.open(path, { read: false }))` opens a read-only database.
The staging index uses `{ readOnly: false }`. Do not open existing database files
with cache `write: true`, which creates a stream that can truncate the file.

## Files and validation

- `shiragame.ts`: exported lifecycle and scrape callbacks.
- `content.ts`: persistent generation state and publication.
- `archive.ts`: bounded download and native ZIP extraction.
- `catalog.ts`: schema validation and index creation.
- `matching.ts`, `serial.ts`: platform-scoped matching and serial normalization.

Run the SQLite-backed plugin fixtures:

```sh
node --experimental-vm-modules --test plugins/shiragame/tests/*.test.mjs
cargo test -p polymer-pluginhost -p polymer-scraping --lib --tests
```

Two optional integration tests use the real release. Set `SHIRAGAME_TEST_DB` to an
extracted `shiragame.db` or `SHIRAGAME_TEST_ZIP` to its archive, then run the matching
ignored test:

```sh
cargo test -p polymer-pluginhost --lib shiragame_release_indexes_and_matches_a_real_dump -- --ignored --nocapture
cargo test -p polymer-pluginhost --lib shiragame_release_zip_extracts_through_the_plugin_cache -- --ignored --nocapture
```

Sources: [database schema](https://github.com/SnowflakePowered/shiratsu/blob/master/SPECIFICATION.md)
and [serial normalization implementation](https://github.com/SnowflakePowered/shiratsu/blob/master/shiratsu-dat/src/common/serial.rs).
