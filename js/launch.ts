import type { ConfigurationApi, ConfigurationSelection, EmulatorApi, LaunchInstance, LaunchOptions, LaunchRequest, LibraryApi, PackageConfiguration } from "./polymer";

const WII = "NINTENDO_WII";
const WII_REMOTE = "WII_REMOTE_CONTROLLER";

export type ResolvedGame = { launchableId: string; draft: PackageConfiguration };
export type AudioState = Readonly<{ available: boolean; volume: number; muted: boolean }>;
export type LaunchHandlers = {
  ready(): void;
  audio(state: AudioState): void;
  surfaces(): void;
  /** The instance closed, with an error message when it failed. */
  finished(error?: string): void;
};

const ATTACH_FRAMES = 180;

/** Resolves library paths into editable launch configurations and launch requests. */
export class GameLauncher {
  constructor(private polymer: Readonly<{ library: LibraryApi; configuration: ConfigurationApi; emulator: EmulatorApi }>) {}

  async resolve(path: string, emulator?: string, signal?: AbortSignal): Promise<ResolvedGame> {
    const launchable = await this.polymer.library.launchables.find(path);
    if (!launchable) throw new Error("Add this launchable to the library before configuring it");
    if (signal?.aborted) throw signal.reason;
    const draft = await this.polymer.configuration.describe(launchable.id, { emulator });
    if (draft.platform === WII) {
      for (const group of draft.candidates.flatMap(candidate => candidate.groups)) {
        const first = group.ports[0];
        if (group.name === "wii" && first?.layouts.includes(WII_REMOTE)) first.selectedLayout = WII_REMOTE;
      }
    }
    return { launchableId: launchable.id, draft };
  }

  async launch(launchableId: string, game: PackageConfiguration): Promise<LaunchRequest> {
    const emulator = game.candidates.find(item => item.id === game.selectedEmulator) || game.candidates[0];
    if (!emulator) throw new Error(`No emulator is available for ${game.name}`);
    const selection: ConfigurationSelection = {
      emulator: emulator.id,
      groups: emulator.groups.map(group => ({
        name: group.name,
        ports: group.ports.map(port => ({ index: port.index, layout: port.selectedLayout })),
      })),
    };
    const launchable = await this.polymer.library.launchables.find(game.path);
    const library = launchable && await this.polymer.library.games.get(launchable.parentBinaryId ?? launchable.binaryId);
    const discs = (library?.launchables ?? [])
      .filter((disc, index, all) => all.findIndex(other => other.binaryId === disc.binaryId) === index)
      .map(disc => disc.binaryId === launchable?.binaryId ? launchableId : disc.id);
    return this.polymer.configuration.onLaunch(discs.length > 1 ? discs : launchableId, selection);
  }

  /**
   * Launches once every requested surface has a connected tracked-surface element, retrying each
   * frame. Resolves null when cancelled before launch.
   */
  start(request: LaunchRequest, find: (index: number) => HTMLElement | undefined, cancelled: () => boolean): Promise<LaunchInstance | null> {
    return new Promise((resolve, reject) => {
      const attempt = (remaining: number) => requestAnimationFrame(() => {
        if (cancelled()) return resolve(null);
        try {
          const surfaces = request.surfaces.map(surface => {
            const attach = find(surface.index);
            if (!attach?.isConnected || attach.localName !== "tracked-surface") throw new Error(`Surface ${surface.index} is not ready`);
            return { ...surface, attach };
          });
          navigator.clearImageMipChains();
          resolve(this.polymer.emulator.launch({ ...request, surfaces } as LaunchOptions));
        } catch (error) {
          if (remaining > 0) attempt(remaining - 1);
          else reject(error);
        }
      });
      attempt(ATTACH_FRAMES);
    });
  }

  /** Subscribes to an instance's lifecycle, replaying a state it already reached. */
  watch(instance: LaunchInstance, handlers: LaunchHandlers) {
    void instance.ready.catch(() => {});
    const audio = (event: Event) => handlers.audio((event as CustomEvent<AudioState>).detail);
    const failed = (event: Event) => handlers.finished((event as CustomEvent<Error>).detail?.message || "Game launch failed");
    const closed = () => handlers.finished();
    instance.addEventListener("ready", handlers.ready);
    instance.addEventListener("audioChanged", audio);
    instance.addEventListener("surfacesChanged", handlers.surfaces);
    instance.addEventListener("error", failed);
    instance.addEventListener("closed", closed);
    if (instance.state === "ready") handlers.ready();
    else if (instance.state === "error") handlers.finished(instance.error?.message || "Game launch failed");
    else if (instance.state === "closed") closed();
    return () => {
      instance.removeEventListener("ready", handlers.ready);
      instance.removeEventListener("audioChanged", audio);
      instance.removeEventListener("surfacesChanged", handlers.surfaces);
      instance.removeEventListener("error", failed);
      instance.removeEventListener("closed", closed);
    };
  }
}
