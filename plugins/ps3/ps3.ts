import { caches } from "polymer:cache";
import { download } from "polymer:download";
import type { PlatformDevice, PluginCacheFile, UpdateCandidate, UpdateQuery } from "polymer:api";

const FIRMWARE_URL = "http://dus01.ps3.update.playstation.net/update/ps3/image/us/2026_0318_a2b60b6ac1d2e49e230144345616927c/PS3UPDAT.PUP";

export async function onBootstrap(device: PlatformDevice): Promise<void> {
  const file = caches.firmware.open(`${device.id}/PS3UPDAT.PUP`);
  await download(FIRMWARE_URL, file);
  // Native depot requirements are checked before firmware extraction.
  await device.install(file);
}

/** A patch PKG listed in a title's update list. Its SHA-1 excludes the 32-byte PKG footer. */
type Ps3Patch = { url: string; sha1: string; size: number };

const updateList = (titleId: string) =>
  `https://a0.ww.np.dl.playstation.net/tpl/np/${titleId}/${titleId}-ver.xml`;

/** Orders "01.02"-style versions numerically. */
function compareVersions(left: string, right: string): number {
  const parts = (version: string) => version.split(".").map(Number);
  const [a, b] = [parts(left), parts(right)];
  for (let index = 0; index < Math.max(a.length, b.length); index++) {
    const difference = (a[index] ?? 0) - (b[index] ?? 0);
    if (difference !== 0) return difference;
  }
  return 0;
}

function attributes(element: string): Record<string, string> {
  return Object.fromEntries([...element.matchAll(/(\w+)="([^"]*)"/g)].map(([, name, value]) => [name, value]));
}

export async function* onCheckUpdates(title: UpdateQuery, device: PlatformDevice): AsyncGenerator<UpdateCandidate> {
  // The update host's certificate cannot be verified; the manifest lists it as insecure.
  const response = await fetch(updateList(title.titleId));
  // Titles that never had an update have no list.
  if (response.status === 404) return;
  if (!response.ok) throw new Error(`PS3 update list for ${title.titleId} failed: HTTP ${response.status}`);
  // Update artifact IDs are content IDs, which hold the title ID at offset 7.
  const installed = [title.version, ...device.artifacts
    .filter(artifact => artifact.depotType === "appupdate" && artifact.id.slice(7, 16) === title.titleId)
    .map(artifact => artifact.version)]
    .filter((version): version is string => version !== null)
    .sort(compareVersions)
    .at(-1);
  const xml = await response.text();
  const name = xml.match(/<TITLE>([^<]*)<\/TITLE>/)?.[1];
  const patches = [...xml.matchAll(/<package\s[^>]*>/g)]
    .map(([element]) => attributes(element))
    .filter(patch => patch.version && patch.url && patch.sha1sum && patch.size)
    .filter(patch => installed === undefined || compareVersions(patch.version, installed) > 0)
    .sort((left, right) => compareVersions(left.version, right.version));
  for (const patch of patches) {
    yield {
      titleId: title.titleId,
      version: patch.version,
      label: `${name?.replace(/\s*\d+\.\d+$/, "") ?? title.titleId} ${patch.version}`,
      byteLength: Number(patch.size),
      source: { url: patch.url, sha1: patch.sha1sum, size: Number(patch.size) } satisfies Ps3Patch,
    };
  }
}

export async function onFetchUpdate(candidate: UpdateCandidate): Promise<PluginCacheFile> {
  const patch = candidate.source as Ps3Patch;
  const file = caches.updates.open(`${candidate.titleId}-${candidate.version}.pkg`);
  await download(patch.url, file, { sha1: patch.sha1, byteLength: patch.size, digestLength: patch.size - 32 });
  return file;
}
