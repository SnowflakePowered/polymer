import type { ConsoleStorageVolume } from "../../../../../../js/polymer";

const setups = new Map<string, Promise<ConsoleStorageVolume>>();

export async function selectedNand(platform: string) {
  return localStorage.getItem(`polymer.nand.${platform}`);
}

export async function selectNand(volume: ConsoleStorageVolume) {
  await localStorage.setItem(`polymer.nand.${volume.platform}`, volume.id);
}

export async function resolveNand(platform: string) {
  const id = await selectedNand(platform);
  const volumes = await window.Polymer.consolestorage.Storage.list({ kind: "nand" });
  const volume = volumes.find(item => item.platform === platform && item.id === id) ?? volumes.find(item => item.platform === platform);
  if (volume) return volume;
  throw Object.assign(new Error("Choose an existing NAND, import one, or create a new NAND."), { code: "NAND_REQUIRED" });
}

export async function ensureNand(platform: string) {
  try {
    return await resolveNand(platform);
  } catch (error) {
    if ((error as { code?: string }).code !== "NAND_REQUIRED") throw error;
  }
  let pending = setups.get(platform);
  if (!pending) {
    pending = (async () => {
      const api = window.Polymer.consolestorage;
      const volume = await api.Storage.create("nand", platform, { label: `${window.Polymer.machine(platform)?.name ?? platform} NAND` });
      await selectNand(volume);
      if ((await api.platforms()).some(item => item.platform === platform)) await volume.bootstrap();
      return volume;
    })().finally(() => setups.delete(platform));
    setups.set(platform, pending);
  }
  return pending;
}
