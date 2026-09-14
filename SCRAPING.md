# Scraping

## Hierarchy of scrape keys

### `package`
`package` keys is information derived by the game package.

* `package.platform` - The `consolespec` platform ID derived from the game package
* `package.hash` - The hash of the first executable binary of the package. Requires one of subkey `md5`, `crc32`, `blake3`, `sha1`, `sha256`.
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
are collected separately for constituent files, never from the CUE text.

After metadata scraping settles, the host parses the saved sheet with `cuna` and
stores a template whose FILE and CDTEXTFILE operands refer to manifest members.
Original formatting, comments, track types, and indexes are retained. Preparation
is independent of metadata scrape success; errors are exposed as
`launchable.preparation`. Pending manifests recover on startup, including imports
interrupted before a scrape was queued. Revision-checked writes prevent stale
finalizers from replacing a re-imported manifest.

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
