# Title Update API

Platform plugins may discover and install title updates (patches) for games in the library. This is separate from `onBootstrap`, which only installs system software. System software imports (`PS3UPDAT.PUP`) are described in [FIRMWARE.md](FIRMWARE.md); tickets and title keys obtained while installing an update go to the keystore described in [KEYS.md](KEYS.md).

Updates are checked and fetched for library titles, whether they are installed into a NAND or are loose disc images and folders. Checking and fetching are both jobs. A check is an in-memory `check` job. A fetch is a persistent `update` job that downloads the update as a depot and then queues an ordinary `install` job for it. Files are downloaded with `polymer:download`, which runs each download as a persistent `download` job.

## Plugin exports

A platform plugin supports updates by exporting both functions, directly or on its default export. The host loads every platform plugin when it starts and records which functions each exports, the same way emulator plugins' optional callbacks are discovered, so no manifest entry is needed.

```ts
export async function* onCheckUpdates(
  title: UpdateQuery,
  device: PlatformDevice,
  context: UpdateContext,
): AsyncGenerator<UpdateCandidate, void, void>;

export async function onFetchUpdate(
  candidate: UpdateCandidate,
  context: UpdateContext,
): Promise<PluginCacheFile | PluginCacheDirectory>;
```

`BootstrapDevice` becomes `PlatformDevice` and keeps the same shape: `id`, `platform`, `artifacts`, and `install(source)`. `onBootstrap` receives the same device type.

```ts
export interface UpdateQuery {
  readonly titleId: string;
  /** Version of the base package in the library, if known. */
  readonly version: string | null;
  /** Installed NAND artifact backing the launchable, when the launchable is an artifact. */
  readonly artifactId: string | null;
}

export interface UpdateCandidate {
  readonly titleId: string;
  readonly version: string;
  readonly label: string;
  readonly byteLength: number | null;
  /** Plugin-defined JSON-safe payload returned unchanged to onFetchUpdate. */
  readonly source: unknown;
}

export interface UpdateContext extends FirmwareContext, KeyWriterContext {}
```

### `onCheckUpdates`

It is called once for a single title and yields every update available for it, oldest first. `device` is the NAND the title's updates are installed into: an installed title's own NAND, or the platform's default NAND for a title that is not installed. When the platform has no NAND yet, `device` has no artifacts, so nothing counts as installed. A title can have several, for example each patch PKG in a cumulative PS3 chain. The plugin finds the installed update version by looking for `device.artifacts` entries with `depotType === "appupdate"` that match the title under the platform's own title ID rules (Wii U `0005000E` high word, PS3 title ID, Vita `ux0/patch/<TITLEID>`). It yields only candidates newer than what is installed, and nothing when the title is up to date. A network failure throws instead of yielding nothing.

`onCheckUpdates` must not call `device.install`. The host rejects it during a check.

Plugins parse update lists without `DOMParser`, which the plugin runtime does not provide; the PS3 plugin reads `<package …/>` attributes with a regular expression.

### `onFetchUpdate`

It downloads one candidate into one of the plugin's caches and returns its depot, which the host installs like any other: a single file, such as a PS3 PKG, or a cache directory holding every file of the depot, such as a CDN content set. `cache.directory(path)` returns a directory inside a cache, and files opened in it are created on first write. It downloads with `download` from `polymer:download`, and writes tickets or keys it fetches separately, such as a CDN `cetk`, with `context.keyStore.put`. It does not receive a device and does not install anything. When a title has several candidates, they are fetched and installed in the order `onCheckUpdates` yielded them.

```ts
import { download } from "polymer:download";

export async function onFetchUpdate(candidate) {
  const { url, sha1, size } = candidate.source as { url: string; sha1: string; size: number };
  const file = caches.updates.open(`${candidate.titleId}-${candidate.version}.pkg`);
  // The listed SHA-1 covers the package without its 32-byte footer.
  await download(url, file, { sha1, byteLength: size, digestLength: size - 32 });
  return file;
}
```

An update job that is interrupted before it queues the install runs `onFetchUpdate` again from the start. Because downloads resume, files that were already downloaded are not downloaded again, so `onFetchUpdate` must be written to be repeatable: open cache files without `truncate`, and let `download` decide what to keep. The depot stays in the plugin cache after it is installed.

Each plugin declares its update hosts in `[plugin.permissions.net] allowed-hosts` and an `updates` entry in `[[plugin.caches]]`.

### Hosts with unverifiable certificates

Some update hosts present certificates that cannot be verified. Sony's PS3 update host has a certificate without a subjectAltName, which the TLS stack never matches against a host name, signed by a chain that uses SHA-1. A plugin lists such hosts in `insecure-hosts`, and their certificates are not verified by the plugin's `fetch` or by `download`:

```toml
[plugin.permissions.net]
allowed-hosts = ["a0.ww.np.dl.playstation.net", "b0.ww.np.dl.playstation.net"]
insecure-hosts = ["a0.ww.np.dl.playstation.net"]
```

Every insecure host must also be an allowed host, and every other host is still verified. Requests to an insecure host can be intercepted, so a plugin should rely on it only for data it can check another way. For the PS3 update list, an attacker who can intercept the request can substitute the listed package and its SHA-1, so this trades authenticity of the list for reachability.

### `polymer:download`

Any plugin can download a file into one of its caches:

```ts
import { download } from "polymer:download";

export interface DownloadOptions {
  /** Expected size of the complete file. */
  readonly byteLength?: number;
  /** Expected digest of the complete file. */
  readonly sha1?: string;
  readonly sha256?: string;
  /** The digest covers only the file's first `digestLength` bytes. */
  readonly digestLength?: number;
  /** The download fails once the file grows past this many bytes. */
  readonly maxByteLength?: number;
}

download(url: string, file: PluginCacheFile, options?: DownloadOptions): Promise<void>;
```

The host performs the transfer as a persistent `download` job, so plugins get resumption and verification without writing them.

- **Jobs.** Each call enqueues a `download` job for the cache file, deduplicated by plugin and file. When a download of the same file is already running, the call waits for that job instead of starting another. When the process restarts, interrupted download jobs are requeued, and the next call for the file resumes them. The request is subject to the plugin's `allowed-hosts`, and `file` must belong to one of its declared caches.
- **Progress.** Transferred bytes are reported to the download job as `Downloading <file name>` with `completed` and `total`. Its `parent` is the job that requested the download: the `polymer:jobs` `Job` whose `run` scope called `download`, or else the host's job for the current call, such as an update job.
- **Checkpoint.** The host records `{ url, etag, lastModified, completed }` in the download job's `detail.checkpoint` before each request and every 8 MiB as data arrives.
- **Resume.** When a checkpoint exists and the cache file holds at least `completed` bytes, the host truncates the file to `completed` and requests the rest with `Range: bytes=<completed>-` and `If-Range` set to the recorded `ETag` (or `Last-Modified` when there is no `ETag`). A `206` response is appended. A `200` response means the server's copy changed or ranges are unsupported, so the file is truncated and downloaded from the start.
- **Verification.** When the transfer completes, the host checks the file's size against `byteLength` and the digest of its first `digestLength` bytes (or all of it) against `sha256` or `sha1`. A resumed file that fails verification is discarded and downloaded once more from the start; a fresh download that fails verification rejects the call. A file that already matches its size and digest is not requested at all. Each check is skipped when its option is not given; with no options, any completed transfer is accepted and the file is downloaded again on every call. A transfer that grows past `maxByteLength` is rejected.

## Host

### Jobs

Two job kinds are added to `JobKind`, alongside `download` from `polymer:download`:

| Kind | Persistent | Params | Result |
|---|---|---|---|
| `check` | No | `{ platform, storage, launchable, title: UpdateQuery }` | `detail.candidates: UpdateCandidate[]` |
| `update` | Yes | `{ platform, storage, launchable, candidate: UpdateCandidate }` | `detail.depot`: the downloaded depot, a file or directory; `detail.install`: the child `install` job |

Both are run by `StorageActor` and carry the launchable's binary as their subject. `storage` is the installed title's NAND, or null for a title that is not installed. A `check` job is deduplicated by launchable while active. An `update` job is deduplicated by `(platform, titleId, version)`.

An `update` job first runs `onFetchUpdate`. It then picks the NAND to install into: its `storage`, or else the platform's default NAND, which is created and bootstrapped as a child `bootstrap` job when the platform has none, the same way imports do. The chosen NAND is written back to `storage`. Finally it queues a child `install` job for the depot, records it in `detail.install`, and completes; the install runs like any other. An `update` job that a previous process left running is resumed; if it had already queued its install, it completes without downloading again. Retrying a failed `update` job runs it again in place.

### Pluginhost

`runtime/platform/updates.rs` sits next to `bootstrap.rs` and adds `PluginRuntimeHandle::check_updates` and `PluginRuntimeHandle::fetch_update`. Each takes the job it reports on, sets up `WorkerOptions` with `firmware.platform` and `keys.*`, allowing key writes only while fetching, and honors `PluginCancellationToken`. Neither sets a NAND target, so the plugin cannot install. `00_platform.js` gains `checkUpdates`, which builds a device whose `install` throws, and `fetchUpdate`, which resolves the returned cache file or directory to the depot's path with `op_platform_depot`.

The host's job is the root `Job` scope of the invocation, so the `download` jobs `onFetchUpdate` starts are children of the `update` job, as are its `bootstrap` and `install` jobs. Frontends show an update job with its children nested under it.

`PluginRuntimeHandle::exports` returns the functions a plugin exports, recorded when the plugin first loads, and `supports_updates` checks for both functions. `StoragePlatform` gains `updates: boolean` from it, so frontends know which platforms can be checked.

### Locking

An `update` job never holds the NAND lease (`ExclusiveResourceKey::Resource`): it only downloads. Its child `install` job takes the lease like any other install, so a game can keep running while its update downloads, and the install fails with `busy` if the game is still running when the download finishes.

### Messages

`polymer-deno/jobs/messages.rs` gains two messages handled by `JobActor`, which reads the launchable from the catalog, builds the `UpdateQuery` from its title ID and artifact, starts the job and returns it:

```rust
#[rtype(result = "Result<Job, JobsError>")]
pub struct JobsCheckUpdates { pub launchable: String }

#[rtype(result = "Result<Job, JobsError>")]
pub struct JobsFetchUpdate { pub launchable: String, pub candidate: UpdateCandidate }
```

### Frontend API

```ts
interface LibraryApi {
  readonly updates: {
    /** Starts a `check` job for a library title; its completed detail holds `candidates`. */
    check(launchableId: string): Promise<Job>;
    /** Starts an `update` job that downloads one candidate as a depot and queues its install. */
    fetch(launchableId: string, candidate: UpdateCandidate): Promise<Job>;
  };
}
```

`JobKind` gains `"check"` and `"update"`. Horizon reports `update` jobs through its notification UI like `install` jobs. `check` jobs are not announced; a completed check's candidates are shown through each launchable's `updates`.

### Triggers and cache

A check is queued for a title in two cases:

- **After an import.** When an `import` job completes, it queues a `check` job for each launchable it added or installed whose platform supports updates.
- **At startup.** When the runtime starts, it queues a `check` job for every library title on a platform whose plugin provides updates. Because check jobs are deduplicated, a title that is already being checked is skipped.

Results go into the `title_updates` table in `library.db`, keyed by launchable, with each candidate's JSON, its position in the order the check returned, and a `checked_at` timestamp. Rows are deleted with their launchable. A successful check replaces every row for that launchable, so an installed update stops being offered at the next check. Each launchable in `LibraryGame.launchables` carries `updates: readonly UpdateCandidate[]` from this table, so the UI never has to ping the server to show that an update is available, and Horizon refreshes the library when a `check` job completes. Updates are never downloaded automatically.

The NAND a title's updates go into is chosen in this order:

1. `LaunchableUri::Artifact` uses its `storage`.
2. Otherwise, the platform's default NAND, its oldest, which launches also use by default.
3. When the platform has no NAND, a check sees no installed updates, and a fetch creates and bootstraps the default NAND.

## Platform support

| Platform | Update source | Installer input today | Status |
|---|---|---|---|
| PS3 | `<TITLEID>-ver.xml` → update PKG with size and SHA-1 | PKG (`ps3/depots/pkg`, `AppUpdate` via flags) | Implemented |
| 3DS | CDN TMD + contents + `cetk` | CIA | Needs a CDN-directory depot or CIA assembly |
| Wii U | NUS TMD + `.app`/`.h3` + `cetk` | Decrypted `meta.xml` layout only | Needs a native NUS depot |
| Vita | Update-list XML → PKG | Decrypted `sce_sys/param.sfo` tree only; PKG/PFS decryption is deliberately separate | Blocked on PKG decryption |
| Xbox 360 | Title updates | No `impls/xbox360` NAND | Out of scope |

### Required consolestorage work

- **Wii U / 3DS:** a depot that installs a CDN directory (TMD, `cetk`, encrypted contents) directly. It decrypts contents natively using the title key that `wiiu/ticket.rs` / `ctr/ticket.rs` already recover from the ticket, and reports through `InstallContext` like every other depot. The plugin downloads the files into a cache directory and returns the directory as the depot. Plugins have no WebCrypto, so decryption cannot happen in TypeScript. TMD content hashes give `download` an expected SHA-1 or SHA-256 for each content.
- **Vita:** a PKG depot, or a documented reason why updates stay manual.

## To verify

- Whether each update host other than PS3's honors `Range` and `If-Range`; a host that ignores them still works, but every resume restarts the file. PS3's package host honors both and sends an `ETag`.
- Whether the Vita update-list URL needs an HMAC. If it does, the host has to provide the primitive, because `Digestible` only covers fixed algorithms over stored bytes.
- Update hostnames for each platform before they are added to `allowed-hosts`.
