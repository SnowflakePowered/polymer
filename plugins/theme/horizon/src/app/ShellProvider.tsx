import { ShellContext } from "./shell-context";
import { LibraryState } from "@polymer-fe/frontend/library-state";
import { useNavigate } from "@solidjs/router";
import { batch, createEffect, onCleanup, onMount, type ParentProps } from "solid-js";
import { createStore, reconcile } from "solid-js/store";
import type { LaunchInstance, LaunchRequest, PackageConfiguration } from "@polymer-fe/frontend";
import type { ControllerDevice } from "@polymer-fe/frontend";
import { NotificationProvider } from "../components/notifications/NotificationProvider";
import { ToastRegion } from "../components/notifications/ToastRegion";
import { InputNavigation } from "../components/InputNavigation";
import { ColorModeProvider, type ColorMode } from "../ui";
import { GameLauncher } from "@polymer-fe/frontend/launch";
import { PlatformShaders } from "@polymer-fe/frontend/shaders";
import { StartupRequests, type StartupRequest } from "@polymer-fe/frontend/startup";
import type { LibraryQueryState, RecentGame, ShellModel } from "./models";
import "./ShellProvider.css";

const RECENT_KEY = "polymer.recentGames.v2";
const COLOR_MODE_KEY = "polymer.horizon.colorMode";
const RECENT_LIMIT = 100;
const DEFAULT_AUDIO = { available: false, volume: 1, muted: false };

const startup = new StartupRequests(window.Polymer.scraping);
import.meta.hot?.dispose(() => startup.dispose());

async function readRecent(): Promise<RecentGame[]> {
  try {
    const stored = JSON.parse(await localStorage.getItem(RECENT_KEY) || "null");
    return stored?.version === 2 && Array.isArray(stored.entries)
      ? stored.entries.slice(0, RECENT_LIMIT)
      : [];
  } catch {
    return [];
  }
}

async function readColorMode(): Promise<ColorMode> {
  try {
    return await localStorage.getItem(COLOR_MODE_KEY) === "dark" ? "dark" : "light";
  } catch {
    return "light";
  }
}

export function ShellProvider(props: ParentProps) {
  const navigate = useNavigate();
  const [model, updateModel] = createStore<ShellModel>({
    launcher: { kind: "idle" },
    launchableId: null,
    draft: null,
    request: null,
    surfaces: [],
    launch: window.Polymer.emulator.activeLaunch,
    audio: { ...DEFAULT_AUDIO },
    error: "",
    recent: [],
    libraryGames: [],
    libraryTotal: 0,
    libraryFiltered: false,
    libraryQuery: { text: "", platforms: [], sort: "title" },
    librarySearchOpen: false,
    closingGame: null,
    libraryActivities: [],
    selectedGame: null,
    gamepadOpen: false,
    cursorVisible: false,
    pauseOpen: false,
    shader: { preset: null, enabled: true },
    controllerToast: null,
    colorMode: "light",
  });
  const setModel = (...path: any[]) => (updateModel as (...args: any[]) => void)(...path);
  let recentLoaded = false;
  let colorModeLoaded = false;
  onMount(() => {
    void readColorMode().then((mode) => {
      colorModeLoaded = true;
      setModel("colorMode", mode);
    });
    if (model.launch) navigate(gamePath("play"), { replace: true });
    void readRecent().then((recent) => {
      recentLoaded = true;
      setModel("recent", recent);
    });
  });
  const games = new GameLauncher(window.Polymer);
  const shaders = new PlatformShaders();
  let detection: AbortController | null = null;
  let closingLaunch: LaunchInstance | null = null;
  let toastTimer: ReturnType<typeof setTimeout> | undefined;

  const go = (path: string, replace = false) => navigate(path, { replace });
  const gamePath = (page: "launching" | "play" | "reorder") => {
    const request = model.request ?? model.launch?.request;
    const platform = request?.machine ?? model.draft?.platform ?? "";
    const game = (request ? request.launchSet?.[request.initialDiscIndex ?? 0]?.id : model.launchableId) ?? "external";
    return `/game/${encodeURIComponent(platform)}/${encodeURIComponent(game)}/${page}`;
  };
  const fail = (error: unknown) => {
    const message = error instanceof Error ? error.message : String(error);
    setModel("error", message);
    setModel("launcher", { kind: "error", message });
    const code = (error as { code?: string })?.code;
    if (["PREFLIGHT_BLOCKED", "PLATFORM_UNAVAILABLE", "NAND_REQUIRED"].includes(code || "") && model.draft) {
      go(`/storage?platform=${encodeURIComponent(model.draft.platform)}`, true);
    } else go("/", true);
  };

  const showControllerToast = (source: ControllerDevice, index: number) => {
    if (toastTimer) clearTimeout(toastTimer);
    setModel("controllerToast", { name: source.name, index });
    toastTimer = setTimeout(() => setModel("controllerToast", null), 3200);
  };
  let knownPlayers = new Map(window.Polymer.controllers.players.flatMap((player) =>
    player.controller ? [[player.controller.id, player.index] as const] : []
  ));
  const playersChanged = () => {
    const next = new Map<string, number>();
    for (const player of window.Polymer.controllers.players) {
      if (!player.controller) continue;
      next.set(player.controller.id, player.index);
      if (knownPlayers.get(player.controller.id) !== player.index) {
        showControllerToast(player.controller, player.index);
      }
    }
    knownPlayers = next;
  };
  window.Polymer.controllers.addEventListener("playerchange", playersChanged);

  const prepare = async (path: string, emulator: string | undefined, destination: "details" | "launch") => {
    const active = window.Polymer.emulator.activeLaunch;
    if (active && active !== closingLaunch) {
      setModel("error", "Close the active game before opening another one");
      return;
    }
    if (active) await active.closed.catch(() => {});
    detection?.abort();
    const preparation = new AbortController();
    detection = preparation;
    setModel("error", "");
    setModel("launcher", { kind: "detecting", name: path.split(/[\\/]/).pop() || path, path });
    if (destination === "launch") {
      setModel("request", null);
      setModel("surfaces", []);
      go("/game/launching");
    }
    try {
      const resolved = await games.resolve(path, emulator, preparation.signal);
      if (preparation.signal.aborted) return;
      setModel("launchableId", resolved.launchableId);
      setModel("draft", resolved.draft);
      if (destination === "details") {
        setModel("launcher", { kind: "details" });
        go("/details");
      } else {
        await beginLaunch(resolved.launchableId, resolved.draft, preparation.signal);
      }
    } catch (error) {
      if (!preparation.signal.aborted) fail(error);
    }
  };

  const beginLaunch = async (launchableId: NonNullable<ShellModel["launchableId"]>, draft: PackageConfiguration, signal?: AbortSignal) => {
    setModel("request", null);
    setModel("launcher", { kind: "launching", name: draft.name });
    go(gamePath("launching"), true);
    try {
      const request = await games.launch(launchableId, draft);
      if (signal?.aborted) return;
      setModel("surfaces", structuredClone(request.surfaces));
      setModel("request", request);
    } catch (error) {
      if (!signal?.aborted) fail(error);
    }
  };

  const launchDraft = (draft = model.draft || undefined) => {
    if (model.launchableId && draft) void beginLaunch(model.launchableId, draft);
  };
  const launchRecent = (game: RecentGame) => { if (model.closingGame !== game.path) void prepare(game.path, game.emulator || undefined, "launch"); };
  const showDetails = (game: RecentGame) => void prepare(game.path, game.emulator || undefined, "details");
  const cancelDetails = () => {
    detection?.abort();
    setModel("launcher", { kind: "idle" });
    setModel("launchableId", null);
    setModel("draft", null);
    setModel("error", "");
    go("/", true);
  };
  const closeGame = () => {
    const launch = model.launch;
    if (!launch) return;
    closingLaunch = launch;
    const path = model.draft?.path ?? null;
    shaders.stop();
    batch(() => {
      setModel("closingGame", path);
      setModel("request", null);
      setModel("launch", null);
      setModel("surfaces", []);
      setModel("pauseOpen", false);
      setModel("gamepadOpen", false);
      setModel("launcher", { kind: "idle" });
    });
    go("/", true);
    const closed = () => { if (model.closingGame === path) setModel("closingGame", null); };
    launch.closed.then(closed, closed);
    launch.close();
  };
  const rememberGame = () => {
    const game = model.draft;
    if (!game) return;
    const emulator = game.selectedEmulator || game.candidates[0]?.id || "";
    const recent: RecentGame = {
      path: game.path,
      name: game.name,
      platform: game.platform,
      emulator,
      playedAt: Date.now(),
    };
    setModel("recent", (entries: RecentGame[]) => [
      recent,
      ...entries.filter((entry) => entry.path !== recent.path || entry.emulator !== recent.emulator),
    ].slice(0, RECENT_LIMIT));
  };

  const libraryState = new LibraryState(window.Polymer.library, window.Polymer.jobs, snapshot => {
    setModel("libraryGames", reconcile(snapshot.games, { key: "path" }));
    setModel("libraryTotal", snapshot.total);
    setModel("libraryFiltered", snapshot.filtered);
    setModel("libraryActivities", reconcile(snapshot.activities, { key: "id" }));
  }, fail);
  libraryState.start();
  onCleanup(() => libraryState.dispose());
  let queryTimer: ReturnType<typeof setTimeout> | undefined;
  const queryLibrary = (query: Partial<LibraryQueryState>) => {
    setModel("libraryQuery", query);
    clearTimeout(queryTimer);
    queryTimer = setTimeout(() => {
      const { text, platforms, sort } = model.libraryQuery;
      libraryState.setQuery({ text, platforms: [...platforms], sort, descending: sort === "added" });
    }, 200);
  };
  const loadMoreGames = () => libraryState.loadMore();

  const acceptStartup = (request: StartupRequest) => {
    if (request.kind === "scrape") {
      setModel("error", "");
      void libraryState.import(request.path);
      return;
    }
    if (request.kind === "package") {
      void prepare(request.path, request.emulator, "launch");
      return;
    }
    setModel("request", request.options);
    setModel("surfaces", structuredClone(request.options.surfaces));
    setModel("launcher", { kind: "launching", name: request.options.command });
    go(gamePath("launching"), true);
  };
  onCleanup(startup.accept(acceptStartup));

  createEffect(() => {
    const entries = model.recent;
    if (recentLoaded) {
      void Promise.resolve(localStorage.setItem(RECENT_KEY, JSON.stringify({ version: 2, entries })))
        .catch(() => {});
    }
  });
  createEffect(() => {
    const mode = model.colorMode;
    if (colorModeLoaded) {
      void Promise.resolve(localStorage.setItem(COLOR_MODE_KEY, mode)).catch(() => {});
    }
  });
  onCleanup(() => {
    detection?.abort();
    if (toastTimer) clearTimeout(toastTimer);
    clearTimeout(queryTimer);
    window.Polymer.controllers.removeEventListener("playerchange", playersChanged);
  });

  return (
    <ShellContext.Provider value={{
      shaders,
      model,
      setModel,
      navigate: go,
      gamePath,
      launchRecent,
      showDetails,
      launchDraft,
      cancelDetails,
      closeGame,
      rememberGame,
      queryLibrary,
      loadMoreGames,
      showcase: path => libraryState.showcase(path),
    }}>
      <ColorModeProvider mode={model.colorMode}><NotificationProvider><div class="horizon-shell-root">
        <div class="horizon-shell-content">{props.children}</div>
        <InputNavigation />
        <ToastRegion />
      </div></NotificationProvider></ColorModeProvider>
    </ShellContext.Provider>
  );
}
