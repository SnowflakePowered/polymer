import { unwrap } from "solid-js/store";
import type {
  ConfigurationSelection,
  EmulatorConfiguration,
  PackageConfiguration,
} from "../../../../../js/polymer";

const WII = "NINTENDO_WII";
const WII_REMOTE = "WII_REMOTE_CONTROLLER";

export function cloneGame(game: PackageConfiguration) {
  return structuredClone(unwrap(game));
}

export function applyFrontendDefaults(game: PackageConfiguration) {
  if (game.platform !== WII) return game;
  for (const emulator of game.candidates) {
    for (const group of emulator.groups) {
      if (group.name !== "wii") continue;
      const first = group.ports[0];
      if (first?.layouts.includes(WII_REMOTE)) first.selectedLayout = WII_REMOTE;
    }
  }
  return game;
}

export async function resolveGame(path: string, emulator?: string, signal?: AbortSignal) {
  const games = await window.Polymer.library.games.list();
  const launchable = games.flatMap(game => game.launchables).find(item => item.uri === path);
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
      ports: group.ports.map((port) => ({
        index: port.index,
        layout: port.selectedLayout,
      })),
    })),
  };

  return selection;
}

export async function onLaunch(launchableId: string, game: PackageConfiguration) {
  return window.Polymer.configuration.onLaunch(launchableId, await createSelection(game));
}
