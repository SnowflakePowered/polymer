# Title Update API

Platform plugins may discover and install title updates (patches) for games in the library. This is separate from `onBootstrap`, which only installs system software. System software imports (`PS3UPDAT.PUP`) are described in [FIRMWARE.md](FIRMWARE.md); tickets and title keys obtained while installing an update go to the keystore described in [KEYS.md](KEYS.md).

## Plugin exports

A platform plugin opts in by exporting both functions. The host treats a plugin that exports neither as not supporting updates. Exporting only one is a load error.

```ts
export async function onCheckUpdates(
  title: UpdateQuery,
  device: PlatformDevice,
  context: UpdateContext,
): Promise<readonly UpdateCandidate[]>;

export async function onFetchUpdate(
  candidate: UpdateCandidate,
  device: PlatformDevice,
  context: UpdateContext,
): Promise<void>;
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

export interface UpdateContext extends PluginProgressContext, KeyWriterContext {}
```

### `onCheckUpdates`

It is called once for a single title and returns every update available for it, oldest first. A title can have several, for example each patch PKG in a cumulative PS3 chain. The plugin finds the installed update version by looking for `device.artifacts` entries with `depotType === "appupdate"` that match the title under the platform's own title ID rules (Wii U `0005000E` high word, PS3 title ID, Vita `ux0/patch/<TITLEID>`). It returns only candidates newer than what is installed, and an empty list when the title is up to date. A network failure rejects the call instead of resolving to an empty list.

`onCheckUpdates` must not call `device.install`. The host rejects `install` during a check.

### `onFetchUpdate`

This follows the same flow as the PS3 `onBootstrap`. It installs one candidate. It streams the download into a declared plugin cache, reports byte progress through `context.progress`, and then calls `device.install(file)`. A candidate made of several files, such as a CDN content set, may call `install` more than once, and those calls run in order. When a title has several candidates, the host fetches them in the order `onCheckUpdates` returned them. The plugin writes tickets or keys it fetches separately, such as a CDN `cetk`, with `context.keyStore.put` before installing.

```ts
export async function onFetchUpdate(candidate, device, context) {
  const { url } = candidate.source as { url: string };
  const file = caches.updates.open(`${candidate.titleId}/${candidate.version}.pkg`, { write: true, truncate: true });
  // stream response.body → file.writable, calling context.progress({ message, completed, total })
  await device.install(file);
}
```

Each plugin declares its update hosts in `[plugin.permissions.net] allowed-hosts` and an `updates` entry in `[[plugin.caches]]`.

## Host

### Pluginhost

`runtime/platform/updates.rs` sits next to `bootstrap.rs` and adds `PluginRuntimeHandle::check_updates` and `PluginRuntimeHandle::fetch_update`. Each one sets up `WorkerOptions` the same way `bootstrap` does: `platform.target`, `firmware.platform`, and `keys.*` with `allow_write`. Each honors `PluginCancellationToken`. `00_platform.js` gains `checkUpdates` and `fetchUpdate` wrappers. They build the device and context the same way `bootstrap` does. `checkUpdates` builds a device whose `install` throws.

`PlatformPluginInstance` exposes `updates() -> bool`, read from the plugin's exports at load time. `StoragePlatform` gains `updates: boolean` so frontends know which platforms can be checked.

### Locking

The NAND lease (`ExclusiveResourceKey::Resource`) is **not** held while checking or downloading. `StorageManager::install` serializes every install through its own queue, and `op_platform_install` takes the lease around each `manager.install` call. Bootstrap already holds the lease for its whole run, so it skips taking it again. As a result, a game can keep running while its update downloads. If an install finds the NAND busy, it fails with `busy` and the downloaded file stays in the plugin cache, so retrying does not download it again.

### Messages and events

`polymer-deno/consolestorage/messages.rs` gains:

```rust
#[rtype(result = "Result<Vec<UpdateCandidate>, StorageError>")]
pub struct CheckUpdates { pub platform: String, pub id: String, pub title: UpdateQuery }

#[rtype(result = "Result<Vec<StorageArtifact>, StorageError>")]
pub struct FetchUpdate { pub platform: String, pub id: String, pub candidate: UpdateCandidate }
```

`FetchUpdate` reports progress on the existing **`installprogress`** channel, so `ConsoleStorageApi.installs` and Horizon's install activity work without changes. The event `filename` is the candidate `label`. The flow is `queued`, then `running` with phase `downloading` (from the plugin's `context.progress`), then `running` with the installer phases, then `complete`/`failed`. `InstallPhase` gains `"downloading"`. `complete` carries the last installed artifact.

### Frontend API

```ts
interface ConsoleStorageVolume {
  readonly updates: {
    check(title: UpdateQuery): Promise<readonly UpdateCandidate[]>;
    fetch(candidate: UpdateCandidate): Promise<readonly StorageArtifact[]>;
  };
}
```

### Library trigger and cache

After `library.add` imports a launchable whose platform supports updates, the runtime queues a background check on that launchable's target NAND. Results go into `<storage>/updates/updates.db`, keyed by `(platform, storage, titleId, version)`, with the candidate JSON, its position in the returned order, and a `checked` timestamp. A successful check replaces every row for that title. A successful fetch deletes that candidate's row. `LibraryGame` exposes `updates: readonly UpdateCandidate[]` from this cache, so the UI never has to ping the server to show that an update is available. Updates are never downloaded automatically.

The target NAND is chosen in this order:

1. `LaunchableUri::Artifact` uses its `storage`.
2. `File` launchables use the NAND in the launchable's last saved `ConfigurationSelection.storage`.
3. Otherwise, the platform's only NAND is used. If the platform has more than one NAND and no selection, the check is skipped until the user picks one.

## Platform support

| Platform | Update source | Installer input today | Status |
|---|---|---|---|
| PS3 | `<TITLEID>-ver.xml` → update PKG | PKG (`ps3/depots/pkg`, `AppUpdate` via flags) | Ready |
| 3DS | CDN TMD + contents + `cetk` | CIA | Needs a CDN-directory depot or CIA assembly |
| Wii U | NUS TMD + `.app`/`.h3` + `cetk` | Decrypted `meta.xml` layout only | Needs a native NUS depot |
| Vita | Update-list XML → PKG | Decrypted `sce_sys/param.sfo` tree only; PKG/PFS decryption is deliberately separate | Blocked on PKG decryption |
| Xbox 360 | Title updates | No `impls/xbox360` NAND | Out of scope |

### Required consolestorage work

- **Wii U / 3DS:** a depot that installs a CDN directory (TMD, `cetk`, encrypted contents) directly. It decrypts contents natively using the title key that `wiiu/ticket.rs` / `ctr/ticket.rs` already recover from the ticket. The plugin downloads the files into a cache directory and calls `device.install` on the directory. Plugins have no WebCrypto, so decryption cannot happen in TypeScript.
- **Vita:** a PKG depot, or a documented reason why updates stay manual.

## To verify

- Whether the PS3 update host serves a certificate chain that the plugin runtime's `fetch` accepts.
- Whether the Vita update-list URL needs an HMAC. If it does, the host has to provide the primitive, because `Digestible` only covers fixed algorithms over stored bytes.
- Update hostnames for each platform before they are added to `allowed-hosts`.
