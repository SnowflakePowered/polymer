# Plugin distribution

Plugins are published from their own repositories. The
[polymer-index](https://github.com/SnowflakePowered/polymer-index) repository lists those
repositories and the keys that sign their releases; it holds no catalog of its own. A repository
describes its plugins and releases in a `Polymer.toml` at its root, and Polymer reads that file
from each listed repository.

## `plugin.toml`

Every plugin declares the host API it is written against:

```toml
[plugin]
id = "melonds"
version = "0.0.1"
class = "emulator"
api = "^0.1"
main = "melonds.ts"
```

An `id` is either a bare name, which only official repositories publish, or `@scope/name` under a
scope the publishing repository claims in polymer-index. Names and scopes use lowercase letters,
digits, `-`, and `_`.

`api` is a semver range. Themes are checked against the version of `@polymer-fe/frontend`, and
every other class against `@polymer-fe/plugin-api`. Polymer refuses to load a plugin whose range
does not include its own version.

## `Polymer.toml`

```toml
[[plugin]]
id = "melonds"
class = "emulator"                # must match plugin.toml
path = "plugins/melonds"          # directory holding plugin.toml
name = "melonDS"
description = "Nintendo DS emulation."
icon = "plugins/melonds/icon.png" # optional
authors = ["…"]
maintainers = ["…"]
homepage = "https://…"            # optional

[[plugin.release]]
version = "0.0.1"
api = "^0.1"
url = "https://…/melonds-0.0.1.asar"
yanked = false
```

Releases exist only because they are listed here. `url` can point at any HTTPS host. Set
`yanked = true` to withdraw a release; entries are never removed, so earlier versions stay
available.

## Signed archives

A release is an asar archive with a minisign signature appended to it:
`[asar][signature][u32 signature length, little-endian]["POLYMSIG"]`. Asar readers ignore the
trailing bytes, so standard asar tools still open the file. The signature's trusted comment names
the plugin and version (`id=melonds version=0.0.1`), so an archive cannot pass as another plugin or
version.

Polymer checks the signature every time it loads an archive: one of the keys registered for the
plugin's repository must have signed it, and the archive's own `plugin.toml` must declare the ID
and version the signature names. Archives that fail are not loaded.

There are no release channels: Polymer offers the newest release that is not yanked and whose API
range it satisfies.

## `polymer-pack`

```sh
polymer-pack keygen --out polymer.key           # prints the public key for the index entry
polymer-pack pack --id melonds --out dist        # writes and prints dist/melonds-0.0.1.asar
polymer-pack sign --key polymer.key dist/melonds-0.0.1.asar
polymer-pack record --id melonds --url https://…/melonds-0.0.1.asar dist/melonds-0.0.1.asar
polymer-pack verify                              # checks Polymer.toml
polymer-pack verify --index polymer.toml --fetch # also downloads and checks every release
polymer-pack check-index path/to/polymer-index   # checks a polymer-index checkout
```

- `pack` validates the plugin as Polymer would load it, then archives it. A theme packs its
  `plugin.toml` and its `theme.root` and `assets` directories, which must be built first. Every
  other plugin packs its directory except `node_modules`, `.git`, and a top-level `bin`. A scoped
  ID's archive is named without the `@` and with `-` for `/`, such as `someone-theme-1.0.0.asar`.
- `sign` appends the signature to the archive in place, so sign before publishing. Without `--key`,
  it reads the secret key from the `POLYMER_SIGNING_KEY` environment variable.
- `record` appends a release to the plugin's section of `Polymer.toml`. It reads the version and
  API range from the signed archive and refuses a version that is already recorded.
- `keygen` writes an unencrypted key so it can be stored as a CI secret. Keep it out of the
  repository; anyone holding it can publish releases of your plugins.

[`support/release-workflow.yml`](support/release-workflow.yml) is a GitHub Actions workflow that
packs, signs, publishes to GitHub Releases, records, and commits the release.

## polymer-index

A repository is registered once, by a pull request adding `repos/<owner>/<name>.toml`:

```toml
repo = "someone/cool-theme"
ref = "main"           # branch or tag Polymer.toml is read from
keys = ["RW…"]         # printed by `polymer-pack keygen`
scopes = ["someone"]   # publishes @someone/… plugins
```

Every repository except official ones must claim at least one scope, and may publish only under the
scopes it claims. A scope is not tied to the GitHub owner. Further pull requests are needed only to
claim scopes or to rotate or revoke keys; archives signed only by a removed key no longer load.

Official repositories publish bare IDs. This repository's entry would be:

```toml
repo = "chyyran/snowglass"
ref = "master"
keys = ["…"]
official = true
```

If a repository is removed from the index, its plugins keep loading with the keys Polymer last
fetched but are no longer updated.

## Where Polymer loads plugins from

Polymer loads every plugin it finds in its search paths: `plugins/` in the working directory, then
`packages/` in its storage directory. Each plugin is either an unpacked directory holding a
`plugin.toml`, or a signed `.asar` archive, and may be nested in a scope directory such as
`@someone/`. Archives load straight from the file. There is no install step: an update downloads
the new archive and moves it into `packages/`, and deleting the file removes the plugin.

Unpacked directories load without a signature and are never updated; this is how plugins are
developed. If an unpacked plugin and an archive have the same ID, the unpacked plugin loads. If
several archives have the same ID, the newest version loads; an update removes the versions it
replaces.
