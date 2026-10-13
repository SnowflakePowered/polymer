import { createContext, createEffect, onCleanup, onMount, useContext, type ParentProps } from "solid-js";
import { createStore, produce, reconcile, unwrap } from "solid-js/store";
import { useNavigate } from "@solidjs/router";
import { LibraryState, type LibraryActivity, type LibraryEntry } from "@polymer-fe/frontend/library-state";
import { GameLauncher } from "@polymer-fe/frontend/launch";
import { NotificationCenter, type Notification } from "@polymer-fe/frontend/notifications";
import { StartupRequests, type StartupRequest } from "@polymer-fe/frontend/startup";
import { PauseRegistry } from "@polymer-fe/frontend/pause";
import { PlatformShaders, type PlatformShader } from "@polymer-fe/frontend/shaders";
import type { EmulatorConfiguration, LaunchInstance, LaunchRequest, PackageConfiguration } from "@polymer-fe/frontend";

const RECENT_KEY = "polymer.vapor.recent";
const RECENT_LIMIT = 30;

export type Phase =
  | { kind: "idle" }
  | { kind: "preparing"; name: string }
  | { kind: "launching"; name: string };
export type LibrarySort = "title" | "platform" | "added";
export const DUAL_SCREEN_PLATFORMS = ["NINTENDO_3DS", "NINTENDO_N3DS"];
export const SCREEN_LAYOUTS = { stacked: "Top and bottom", "side-by-side": "Side by side", hybrid: "Hybrid" } as const;
export type ScreenLayout = keyof typeof SCREEN_LAYOUTS;
export type ShellState = {
  phase: Phase;
  games: LibraryEntry[];
  total: number;
  filtered: boolean;
  activities: LibraryActivity[];
  platforms: string[];
  query: { text: string; platform: string | null; sort: LibrarySort };
  recent: LibraryEntry[];
  launchableId: string | null;
  draft: PackageConfiguration | null;
  request: LaunchRequest | null;
  launch: LaunchInstance | null;
  audio: { available: boolean; volume: number; muted: boolean };
  notifications: Record<string, Notification>;
  menu: "main" | "quick" | "order" | "shader" | "states" | null;
  screenLayout: ScreenLayout;
  shader: PlatformShader;
};

const startup = new StartupRequests(window.Polymer.scraping);
import.meta.hot?.dispose(() => startup.dispose());

const message = (error: unknown) => error instanceof Error ? error.message : String(error);

export class Shell {
  readonly state: ShellState;
  private readonly set: (...args: any[]) => void;
  readonly games = new GameLauncher(window.Polymer);
  readonly pauses = new PauseRegistry();
  readonly shaders = new PlatformShaders();
  readonly library: LibraryState;
  readonly notifications: NotificationCenter;
  private preparation: AbortController | null = null;
  private queryTimer: ReturnType<typeof setTimeout> | undefined;

  constructor(readonly navigate: (path: string, replace?: boolean) => void) {
    const [state, set] = createStore<ShellState>({
      phase: { kind: "idle" }, games: [], total: 0, filtered: false, activities: [], platforms: [],
      query: { text: "", platform: null, sort: "title" }, recent: [],
      launchableId: null, draft: null, request: null, launch: window.Polymer.emulator.activeLaunch,
      audio: { available: false, volume: 1, muted: false }, notifications: {}, menu: null, screenLayout: "stacked",
      shader: { preset: null, enabled: true },
    });
    this.state = state;
    this.set = set as (...args: any[]) => void;
    this.library = new LibraryState(window.Polymer.library, window.Polymer.jobs, snapshot => {
      this.set("games", reconcile(snapshot.games, { key: "path" }));
      this.set("total", snapshot.total);
      this.set("filtered", snapshot.filtered);
      this.set("activities", reconcile(snapshot.activities, { key: "id" }));
    }, this.fail);
    this.notifications = new NotificationCenter(window.Polymer, items => this.set("notifications", reconcile(Object.fromEntries(items.map(item => [item.id, item])))));
  }

  update(...args: any[]) { this.set(...args); }

  readonly fail = (error: unknown) => {
    this.set("phase", { kind: "idle" });
    this.notifications.upsert({ id: "error", title: "Something went wrong", message: message(error), state: "error" });
  };

  query(change: Partial<ShellState["query"]>) {
    this.set("query", change);
    clearTimeout(this.queryTimer);
    this.queryTimer = setTimeout(() => {
      const { text, platform, sort } = this.state.query;
      this.library.setQuery({ text, platforms: platform ? [platform] : [], sort, descending: sort === "added" });
    }, 200);
  }

  async open(game: Pick<LibraryEntry, "path" | "name" | "emulator">, then: "details" | "launch") {
    if (window.Polymer.emulator.activeLaunch) return this.fail(new Error("Close the running game before opening another one"));
    this.preparation?.abort();
    const preparation = this.preparation = new AbortController();
    this.set("phase", { kind: "preparing", name: game.name });
    if (then === "launch") this.navigate("/game");
    try {
      const resolved = await this.games.resolve(game.path, game.emulator || undefined, preparation.signal);
      if (preparation.signal.aborted) return;
      this.set("launchableId", resolved.launchableId);
      this.set("draft", resolved.draft);
      this.set("phase", { kind: "idle" });
      if (then === "launch") await this.launch();
      else this.navigate("/details");
    } catch (error) {
      if (preparation.signal.aborted) return;
      this.fail(error);
      if (then === "launch") this.leave(error);
    }
  }

  async launch() {
    const { launchableId, draft } = this.state;
    if (!launchableId || !draft) return;
    this.set("phase", { kind: "launching", name: draft.name });
    this.navigate("/game", true);
    try {
      this.set("request", await this.games.launch(launchableId, structuredClone(unwrap(draft))));
    } catch (error) {
      this.fail(error);
      this.leave(error);
    }
  }

  private leave(error: unknown) {
    const code = (error as { code?: string })?.code ?? "";
    const platform = this.state.draft?.platform;
    if (platform && ["PREFLIGHT_BLOCKED", "PLATFORM_UNAVAILABLE", "NAND_REQUIRED"].includes(code)) this.navigate(`/storage?platform=${encodeURIComponent(platform)}`, true);
    else this.navigate("/", true);
  }

  started(instance: LaunchInstance) {
    this.set("launch", instance);
    this.set("phase", { kind: "idle" });
    const draft = this.state.draft;
    if (!draft) return;
    const entry: LibraryEntry = { path: draft.path, name: draft.name, platform: draft.platform, emulator: draft.selectedEmulator, playedAt: Date.now() };
    this.set("recent", (recent: LibraryEntry[]) => [entry, ...recent.filter(item => item.path !== entry.path)].slice(0, RECENT_LIMIT));
  }

  finished(error?: string) {
    if (error) this.fail(new Error(error));
    this.set(produce((state: ShellState) => { state.request = null; state.launch = null; state.menu = null; state.phase = { kind: "idle" }; }));
    this.navigate("/", true);
  }

  closeDetails() {
    this.preparation?.abort();
    this.set(produce((state: ShellState) => { state.launchableId = null; state.draft = null; state.phase = { kind: "idle" }; }));
    this.navigate("/", true);
  }

  emulator() {
    const draft = this.state.draft;
    return draft?.candidates.find(item => item.id === draft.selectedEmulator) ?? draft?.candidates[0];
  }

  private editEmulator(id: string, change: (emulator: EmulatorConfiguration) => void) {
    const index = this.state.draft?.candidates.findIndex(item => item.id === id) ?? -1;
    if (index >= 0) this.set("draft", "candidates", index, produce(change));
  }

  selectEmulator(value: string | null) {
    const { draft, launchableId } = this.state;
    if (!draft || !launchableId) return;
    const fallback = draft.candidates.find(item => item.id === draft.platformEmulator)?.id ?? draft.candidates[0]?.id ?? "";
    this.set("draft", { gameEmulator: value, selectedEmulator: value ?? fallback });
    void window.Polymer.configuration.select(launchableId, value).catch(this.fail);
  }

  selectLayout(group: number, port: number, layout: string) {
    const emulator = this.emulator();
    if (emulator) this.editEmulator(emulator.id, candidate => { candidate.groups[group].ports[port].selectedLayout = layout; });
  }

  configure(group: string, option: string, value: boolean | number | string | null) {
    const { launchableId } = this.state;
    const emulator = this.emulator();
    if (!launchableId || !emulator) return;
    const values = structuredClone(unwrap(emulator.userConfiguration)) as Record<string, Record<string, boolean | number | string | null>>;
    (values[group] ||= {})[option] = value;
    this.editEmulator(emulator.id, candidate => { candidate.userConfiguration = values; });
    void window.Polymer.configuration.configure(launchableId, emulator.id, values).then(result => {
      if (result) this.editEmulator(emulator.id, candidate => { candidate.configuration = result.configuration; candidate.userConfiguration = result.userConfiguration; });
    }).catch(this.fail);
  }

  setVolume(volume: number) {
    this.set("audio", "volume", volume);
    this.state.launch?.setVolume(volume);
  }

  readonly accept = (request: StartupRequest) => {
    if (request.kind === "scrape") return void this.library.import(request.path);
    if (request.kind === "package") return void this.open({ path: request.path, name: request.path.split(/[\\/]/).pop() ?? request.path, emulator: request.emulator ?? "" }, "launch");
    this.set("request", request.options);
    this.set("phase", { kind: "launching", name: request.options.command });
    this.navigate("/game", true);
  };

  private players = this.assignedPlayers();

  private assignedPlayers() {
    return new Map(window.Polymer.controllers.players.flatMap(player => player.controller ? [[player.controller.id, player.index] as const] : []));
  }

  readonly playerChanged = () => {
    for (const player of window.Polymer.controllers.players) {
      if (player.controller && this.players.get(player.controller.id) !== player.index) {
        this.notifications.upsert({ id: "controller", title: player.controller.name, message: `Connected as player ${player.index + 1}`, state: "info" });
      }
    }
    this.players = this.assignedPlayers();
  };
}

const ShellContext = createContext<Shell>();

export function ShellProvider(props: ParentProps) {
  const navigate = useNavigate();
  const shell = new Shell((path, replace = false) => navigate(path, { replace }));
  let recentLoaded = false;
  onMount(() => {
    shell.library.start();
    shell.notifications.start();
    void window.Polymer.library.games.platforms().then(platforms => shell.update("platforms", platforms)).catch(() => {});
    void Promise.resolve(localStorage.getItem(RECENT_KEY)).then(stored => {
      const recent = JSON.parse(stored || "[]");
      if (Array.isArray(recent)) shell.update("recent", recent);
    }).catch(() => {}).finally(() => { recentLoaded = true; });
    window.Polymer.controllers.addEventListener("playerchange", shell.playerChanged);
    if (shell.state.launch) navigate("/game", { replace: true });
    onCleanup(startup.accept(shell.accept));
    onCleanup(() => {
      shell.library.dispose();
      shell.notifications.dispose();
      window.Polymer.controllers.removeEventListener("playerchange", shell.playerChanged);
    });
  });
  createEffect(() => {
    const recent = JSON.stringify(shell.state.recent);
    if (recentLoaded) void Promise.resolve(localStorage.setItem(RECENT_KEY, recent)).catch(() => {});
  });
  createEffect(() => {
    const activities = shell.state.activities.map(activity => ({ ...activity }));
    for (const activity of activities) shell.notifications.upsert({ ...activity, id: `library:${activity.id}` });
  });
  return <ShellContext.Provider value={shell}>{props.children}</ShellContext.Provider>;
}

export function useShell() {
  const shell = useContext(ShellContext);
  if (!shell) throw new Error("Vapor shell is unavailable");
  return shell;
}
