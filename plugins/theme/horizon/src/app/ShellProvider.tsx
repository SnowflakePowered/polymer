import { ShellContext } from "./shell-context";
import { LibraryState } from "../../../../../js/library-state";
import { useNavigate } from "@solidjs/router";
import { createEffect, onCleanup, onMount, type ParentProps } from "solid-js";
import { createStore, reconcile } from "solid-js/store";
import type { LaunchRequest, PackageConfiguration } from "../../../../../js/polymer";
import type { ControllerDevice } from "../../../../../js/polymer";
import { NotificationProvider } from "../components/notifications/NotificationProvider";
import { ToastRegion } from "../components/notifications/ToastRegion";
import { InputNavigation } from "../components/InputNavigation";
import { onLaunch, resolveGame } from "./launch";
import type { RecentGame, ShellModel } from "./models";
import "./ShellProvider.css";

const RECENT_KEY = "polymer.recentGames.v2";
const RECENT_LIMIT = 100;
const DEFAULT_AUDIO = { available: false, volume: 1, muted: false };

type StartupRequest =
  | { kind: "scrape"; path: string }
  | { kind: "package"; path: string; emulator?: string }
  | { kind: "launch"; options: LaunchRequest };

const startupQueue: StartupRequest[] = [];
let startupHandler: ((request: StartupRequest) => void) | null = null;
const requestListener = (event: Event) => {
  const request = (event as CustomEvent<StartupRequest>).detail;
  if (startupHandler) startupHandler(request);
  else startupQueue.push(request);
};
window.Polymer.scraping.addEventListener("request", requestListener);
import.meta.hot?.dispose(() => {
  window.Polymer.scraping.removeEventListener("request", requestListener);
});

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
    libraryActivities: [],
    selectedGame: null,
    gamepadOpen: false,
    pauseOpen: false,
    controllerToast: null,
  });
  const setModel = (...path: any[]) => (updateModel as (...args: any[]) => void)(...path);
  let recentLoaded = false;
  onMount(() => {
    if (model.launch) navigate("/game/play", { replace: true });
    void readRecent().then((recent) => {
      recentLoaded = true;
      setModel("recent", recent);
    });
  });
  let detection: AbortController | null = null;
  let toastTimer: ReturnType<typeof setTimeout> | undefined;

  const go = (path: string, replace = false) => navigate(path, { replace });
  const fail = (error: unknown) => {
    const message = error instanceof Error ? error.message : String(error);
    setModel("error", message);
    setModel("launcher", { kind: "error", message });
    go("/", true);
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
    if (window.Polymer.emulator.activeLaunch) {
      fail(new DOMException("Close the active game before opening another one", "InvalidStateError"));
      return;
    }
    detection?.abort();
    const preparation = new AbortController();
    detection = preparation;
    setModel("error", "");
    setModel("launcher", { kind: "detecting", name: path.split(/[\\/]/).pop() || path });
    if (destination === "launch") {
      setModel("request", null);
      setModel("surfaces", []);
      go("/game/launching");
    }
    try {
      const resolved = await resolveGame(path, emulator, preparation.signal);
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
    go("/game/launching", true);
    try {
      const request = await onLaunch(launchableId, draft);
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
  const launchRecent = (game: RecentGame) => void prepare(game.path, game.emulator || undefined, "launch");
  const showDetails = (game: RecentGame) => void prepare(game.path, game.emulator || undefined, "details");
  const cancelDetails = () => {
    detection?.abort();
    setModel("launcher", { kind: "idle" });
    setModel("launchableId", null);
    setModel("draft", null);
    setModel("error", "");
    go("/", true);
  };
  const closeGame = () => model.launch?.close();
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

  const libraryState = new LibraryState(window.Polymer.library, window.Polymer.scraping, snapshot => {
    setModel("libraryGames", reconcile(snapshot.games, { key: "path" }));
    setModel("libraryActivities", reconcile(snapshot.activities, { key: "id" }));
  }, fail);
  libraryState.start();
  onCleanup(() => libraryState.dispose());

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
    go("/game/launching", true);
  };
  startupHandler = acceptStartup;
  for (const request of startupQueue.splice(0)) acceptStartup(request);

  createEffect(() => {
    const entries = model.recent;
    if (recentLoaded) {
      void Promise.resolve(localStorage.setItem(RECENT_KEY, JSON.stringify({ version: 2, entries })))
        .catch(() => {});
    }
  });
  onCleanup(() => {
    detection?.abort();
    if (toastTimer) clearTimeout(toastTimer);
    window.Polymer.controllers.removeEventListener("playerchange", playersChanged);
    if (startupHandler === acceptStartup) startupHandler = null;
  });

  return (
    <ShellContext.Provider value={{
      model,
      setModel,
      navigate: go,
      launchRecent,
      showDetails,
      launchDraft,
      cancelDetails,
      closeGame,
      rememberGame,
    }}>
      <NotificationProvider><div class="horizon-shell-root">
        <div class="horizon-shell-content">{props.children}</div>
        <InputNavigation />
        <ToastRegion />
      </div></NotificationProvider>
    </ShellContext.Provider>
  );
}
