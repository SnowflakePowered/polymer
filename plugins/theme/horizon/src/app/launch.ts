import { unwrap } from "solid-js/store";
import type {
  ConfigurationSelection,
  ConsoleStorageVolume,
  CreatableConsoleStorageKind,
  EmulatorConfiguration,
  PackageConfiguration,
} from "../../../../../js/polymer";

const WII = "NINTENDO_WII";
const WII_U = "NINTENDO_WIIU";
const GAMECUBE = "NINTENDO_GCN";
const DREAMCAST = "SEGA_DC";
const PSX = "SONY_PSX";
const PS2 = "SONY_PS2";
const WII_REMOTE = "WII_REMOTE_CONTROLLER";
const volumeCache = new Map<string, Promise<ConsoleStorageVolume>>();

export function friendly(value: string) {
  return value
    .replace(/^NINTENDO_/, "Nintendo ")
    .replace(/^SONY_/, "Sony ")
    .replace(/^MICROSOFT_/, "Microsoft ")
    .replaceAll("_", " ")
    .replace(/\b\w/g, (letter) => letter.toUpperCase())
    .replace(/Wiiu/g, "Wii U");
}

export function cloneGame(game: PackageConfiguration) {
  return structuredClone(unwrap(game));
}

export function applyFrontendDefaults(game: PackageConfiguration) {
  if (game.platform !== WII) return game;
  for (const emulator of game.candidates) {
    for (const group of emulator.groups) {
      if (group.name === "gamecube") group.connected = 0;
      if (group.name !== "wii") continue;
      group.connected = Math.min(1, group.ports.length);
      const first = group.ports[0];
      if (first?.layouts.includes(WII_REMOTE)) first.selectedLayout = WII_REMOTE;
    }
  }
  return game;
}

async function defaultVolume(kind: CreatableConsoleStorageKind, platform: string) {
  const key = `${kind}:${platform}`;
  let pending = volumeCache.get(key);
  if (!pending) {
    pending = window.Polymer.consolestorage.Storage.list({ kind }).then((items) =>
      items
        .filter((item) => item.platform === platform && item.label === "Default")
        .sort((left, right) => right.created.getTime() - left.created.getTime())[0]
        ?? window.Polymer.consolestorage.Storage.create(kind, platform, { label: "Default" }),
    );
    volumeCache.set(key, pending);
    pending.catch(() => volumeCache.delete(key));
  }
  return pending;
}

export async function resolveGame(path: string, emulator?: string, signal?: AbortSignal) {
  const games = await window.Polymer.library.games.list();
  const launchable = games.flatMap(game => game.launchables).find(item => item.path === path);
  if (!launchable) throw new Error("Add this launchable to the library before configuring it");
  if (signal?.aborted) throw signal.reason;
  const draft = applyFrontendDefaults(await window.Polymer.configuration.describe(launchable.id, { emulator }));
  return { launchableId: launchable.id, draft };
}

export async function createSelection(game: PackageConfiguration): Promise<ConfigurationSelection> {
  const emulator = game.candidates.find((item) => item.id === game.selectedEmulator) || game.candidates[0];
  if (!emulator) throw new Error(`No emulator is available for ${game.name}`);

  const selection: ConfigurationSelection = {
    emulator: emulator.id,
    groups: emulator.groups.map((group) => ({
      name: group.name,
      ports: group.ports.slice(0, group.connected).map((port) => ({
        index: port.index,
        layout: port.selectedLayout,
      })),
    })),
  };

  if (game.platform === WII || game.platform === WII_U) {
    const [save, nand] = await Promise.all([
      defaultVolume("saveRepo", game.platform),
      defaultVolume("nand", game.platform),
    ]);
    selection.storage = { [game.platform === WII_U ? "mlc" : "nand"]: [save.id, nand.id] };
  } else {
    const slots: Record<string, string> = {
      [GAMECUBE]: "slotA",
      [DREAMCAST]: "vmu",
      [PSX]: "card1",
      [PS2]: "mem1",
    };
    const slot = slots[game.platform];
    if (slot) {
      selection.storage = { [slot]: (await defaultVolume("memoryCard", game.platform)).id };
    }
  }
  return selection;
}

export async function onLaunch(launchableId: string, game: PackageConfiguration) {
  return window.Polymer.configuration.onLaunch(launchableId, await createSelection(game));
}
