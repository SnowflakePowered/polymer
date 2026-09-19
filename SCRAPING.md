# Scraping

## Lazy digests

Files and packages expose the same `Digestible` interface:

```ts
type DigestAlgorithm = "sha2" | "sha1" | "md5" | "crc32" | "blake3";
interface Digestible {
  digest(algorithm: DigestAlgorithm): Promise<string>;
}

await file.digest("sha1");            // Complete file bytes
await package_.digest("sha2");        // Complete package file bytes
await package_.binary.digest("blake3"); // First normalized executable
```

Results are lowercase hexadecimal; `sha2` is SHA-256 and CRC32 is eight hex digits.
Directory packages have no whole-file digest and reject `package.digest()`;
their normalized binary can still be hashed. Hashing streams on a blocking worker
and starts when requested. File digests observe current bytes on every call.
Host files, VFS entries (including archive entries and save states), cache files,
and Gumshoe binaries share the Rust `Digestible` trait.
Both Deno realms use one `op_digest`, resolving each source through the realm's
access checks. API boundaries construct package and file digest methods explicitly.

Scrapers receive `onScrape(input)`, with packages in `input.packages` alongside
the existing metadata keys such as `input["package.platform"]`. Each package has `digest()`,
`binary.digest()`, and `files`, whose entries expose `name`, `kind`, `size`,
and `digest()`. CUE backing files are separate entries. Dump candidates
exclude container formats; the container itself remains hashable via
`package.digest()`. Manifest requirements can use `package` or `package.files`.

Library launchables also expose `digest()` and `binary.digest()`, replacing
`binaryBlake3`. Prepared configuration arguments preserve these methods when
passed back to a plugin.

Every newly imported binary, including a child disc, follows the same scrape
pipeline. A job uses only that binary's metadata and launchable packages and
stores its results on that binary. The catalog combines root and child metadata
when presenting a game. Active requests are deduplicated per root and triggering
binary, so importing a child cannot be swallowed by its parent's active scrape.
Unchanged reimports refresh host metadata and package preparation without starting
another scrape; explicit scrape and retry requests remain available.

The old `package.binaryDigest` property and `package.hash.*` / `package.size`
scalar inputs are removed. Legacy hash metadata is filtered from scraper input
and cleared on reimport. Identifying a package no longer hashes its binary;
library import and direct launch compute BLAKE3 when needed for internal identity.

## Hierarchy of scrape keys

### `package`
`package` keys is information derived by the game package.

* `package.platform` - The `consolespec` platform ID derived from the game package
* `package.internalname` - Any internal name of the game package
* `package.serial` - Any internal serial of the game package
* `package.titleid` - Any internal title id of the game package

### `filename`
`filename` keys are information derived by the filename of the ROM or package.

* `filename.raw` - The raw filename of the file.
* `filename.nointro` - If the filename is parseable as NoIntro format, structured information from the filename. Has the subkeys `title`, `version`, `release`, `region`, `media`, `scene`, `language`, `flag`
* `filename.tosec` - If the filename is parseable as NoIntro format, structured information from the filename. Has the subkeys `title`, `version`, `demo`, `date`, `publisher`, `system`, `video`, `region`, `language`, `copyright`, `development`, `dumpinfo`, `media`, `flag`
* `filename.goodtools` - If the filename is parseable as NoIntro format, structured information from the filename. Has the subkeys `title`, `year`, `multilanguage`, `translation`, `version`, `volume`, `ninone`, `dumpcode`, `gamehack`, `media`, `flag`.

### `meta`
`meta` keys are generic metadata that can be sourced via third party databases

* `meta.title` - The title of the game release. Accepts BCP-47 language subkeys.
* `meta.description` - The description of the game release. Accepts BCP-47 language subkeys.
* `meta.publisher` - The publisher of the game release
* `meta.genre` - A genre of the game release.
* `meta.rating` - The age rating of the game release. Requires the subkeys `esrb`, `cero`, `usk`, `pegi`, `dgsc`, `acb`, `iarc`, `grac`, `rars`, `gmedia` (defined as https://www.globalratings.com/ratings-definitions/).
* `meta.releasedate` - The release date of the game. Always in ISO8601 YYYY-MM-dd format.

### `media`
`media` keys are any media sourced via third party databases

* `media.banner` - A horizontal banner, which could be animated.
* `media.boxart` - Boxart images. Requires the subkeys `front`, `back`, `spine`, `full`.
* `media.logo` -  A game title or logo image
* `media.label.disc` - The front label of a disc
* `media.label.cart` - The front label of a game cartridge

### `banner`
`banner` keys are any media or icon derived from the game package.

* `banner.icon` - A game icon embedded in the package
* `banner.background` - A game background embedded in the package
* `banner.logo` - A game title or logo image embedded in the package
* `banner.audio` - An audio file embedded in the package
* `banner.banner` - A horizontal banner embedded in the package, which could be animated.
* `banner.3d` - A 3D model representing the game embedded in the package

### `runtime`
`runtime` keys are information that is derived possibly from third party sources but may be needed to run the game.

* `media.cue` - An optional scraped cuesheet candidate. This does not replace the descriptor of an identified package.

## BIN/CUE package ingestion

`polymer-scraping::Ingestor` accepts file paths or package directories. Individual
files are passed to Gumshoe 0.0.5-beta without neighboring-file lookup. A directory
with CUE sheets is enumerated by sheet; a directory without sheets retains normal
Gumshoe directory identification. An incomplete BIN is rejected by Gumshoe.

Gumshoe identifies a complete disc and exposes its descriptor and dependencies
through `package_files()`. Polymer stores the explicit descriptor path, original
sheet, member paths, sizes, and modification signatures on the launchable. The
normalized executable digest remains the binary identity. Raw dump fingerprints
are requested lazily from constituent file objects, independently of the CUE text.

During identification, the host parses the captured sheet with `cuna` and
builds a template whose FILE and CDTEXTFILE operands refer to manifest members.
Original formatting, comments, track types, and indexes are retained. Preparation
is independent of metadata scraping; the template or preparation error is stored
atomically with the launchable during import and exposed as
`launchable.preparation`. Older pending manifests can still be prepared at launch
or refreshed by reimporting the package.

At launch, the host checks member availability and modification signatures and
renders a CUE using deterministic filenames. Emulator plugins call
`filesystem.mountPackage("/Game")`
to mount the descriptor and dependencies read-only and receive the emulator-visible
entrypoint. The host captures the launch entry in the filesystem closure; a
disc-change callback receives the requested disc, and later callbacks receive the
active disc. Without a CUE bundle, the host mounts a standalone image at
`/Game/<filename>` or an extracted package directory at `/Game`, returning that
file or directory respectively. Dolphin disc images and Cemu packages use the
same API; installed NAND titles continue to boot through console storage.
One disc remains one launchable regardless of track count. Quick
resume identity includes the manifest revision. Existing BIN-based library entries
must be re-imported using their CUE or package directory to acquire a manifest.

No file-set discovery callback, hash-based sibling search, or missing-CUE repair
is part of this pipeline. Scraped `media.cue` is ordinary metadata and does not
control package launch preparation.

Validation:

```text
cargo test -p polymer-library -p polymer-scraping --lib --tests
cargo test -p polymer-pluginhost --test package_mount --test disc_change
```

The ignored `cue_packages` integration test reads a complete local disc from
`POLYMER_CUE_DIRECTORY`, imports it into a temporary library, and verifies CUE and
directory identity, dependency capture, track rejection, and launch preparation.
It does not modify the supplied game directory.
