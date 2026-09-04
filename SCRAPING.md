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

* `runtime.cuesheet` - A cuesheet for the game, if it requires one.