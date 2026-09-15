import type { ConsoleStorageVolume } from "../../../../../../js/polymer";

export function selectedNand(platform: string) {
  return localStorage.getItem(`polymer.nand.${platform}`);
}

export function selectNand(volume: ConsoleStorageVolume) {
  localStorage.setItem(`polymer.nand.${volume.platform}`, volume.id);
}

export async function resolveNand(platform: string) {
  const id = selectedNand(platform);
  const volumes = await window.Polymer.consolestorage.Storage.list({ kind: "nand" });
  const volume = volumes.find(item => item.platform === platform && item.id === id);
  if (volume) return volume;
  throw Object.assign(new Error("Choose an existing NAND, import one, or create a new NAND."), { code: "NAND_REQUIRED" });
}
