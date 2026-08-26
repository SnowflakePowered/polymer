import { useNavigate } from "@solidjs/router";
import { createContext, createEffect, onCleanup, useContext, type ParentProps } from "solid-js";
import { createStore } from "solid-js/store";
import type { LaunchRequest, PackageConfiguration } from "../../../../../js/snowglass";
import type { InputSource } from "../../../../../js/snowglass";
import { ControllerConnectionToast } from "../components/ControllerConnectionToast";
import { InputNavigation } from "../components/InputNavigation";
import { resolveGame, setupLaunch } from "./launch";
import type { RecentGame, ShellModel } from "./models";
import "./ShellProvider.css";

const RECENT_KEY = "snowglass.recentGames.v2";
const RECENT_LIMIT = 100;
const DEFAULT_AUDIO = { available: false, volume: 1, muted: false };

type StartupRequest =
  | { kind: "package"; path: string; emulator?: string }
  | { kind: "launch"; options: LaunchRequest };

const startupQueue: StartupRequest[] = [];
let startupHandler: ((request: StartupRequest) => void) | null = null;
window.Snowglass.scraping.addEventListener("request", (event: Event) => {
  const request = (event as CustomEvent<StartupRequest>).detail;
  if (startupHandler) startupHandler(request);
  else startupQueue.push(request);
});

function readRecent(): RecentGame[] {
  try {
    const stored = JSON.parse(localStorage.getItem(RECENT_KEY) || "null");
    return stored?.version === 2 && Array.isArray(stored.entries)
      ? stored.entries.slice(0, RECENT_LIMIT)
      : [];
  } catch {
    return [];
  }
}

export type ShellContextValue = {
  model: ShellModel;
  setModel: (...path: any[]) => void;
  navigate(path: string, replace?: boolean): void;
  launchRecent(game: RecentGame): void;
  showDetails(game: RecentGame): void;
  launchDraft(draft?: PackageConfiguration): void;
  cancelDetails(): void;
  closeGame(): void;
  rememberGame(): void;
};

const ShellContext = createContext<ShellContextValue>();

export function useShell() {
  const context = useContext(ShellContext);
  if (!context) throw new Error("Horizon shell context is unavailable");
  return context;
}

export function ShellProvider(props: ParentProps) {
  const navigate = useNavigate();
  const [model, updateModel] = createStore<ShellModel>({
    launcher: { kind: "idle" },
    package: null,
    draft: null,
    request: null,
    surfaces: [],
    launch: window.Snowglass.emulator.activeLaunch,
    audio: { ...DEFAULT_AUDIO },
    error: "",
    recent: readRecent(),
    selectedGame: null,
    gamepadOpen: false,
    pauseOpen: false,
    controllerToast: null,
  });
  const setModel = (...path: any[]) => (updateModel as (...args: any[]) => void)(...path);
  let detection: AbortController | null = null;
  let toastTimer: ReturnType<typeof setTimeout> | undefined;
  let assignmentQueue = Promise.resolve();

  const go = (path: string, replace = false) => navigate(path, { replace });
  const fail = (error: unknown) => {
    const message = error instanceof Error ? error.message : String(error);
    setModel("error", message);
    setModel("launcher", { kind: "error", message });
    go("/", true);
  };

  const showControllerToast = (source: InputSource, index: number) => {
    if (toastTimer) clearTimeout(toastTimer);
    setModel("controllerToast", { name: source.name, index });
    toastTimer = setTimeout(() => setModel("controllerToast", null), 3200);
  };
  const assignController = (source: InputSource) => {
    assignmentQueue = assignmentQueue.then(async () => {
      const existing = window.Snowglass.input.assignments.find((assignment) =>
        assignment.source.kind === "device" && assignment.source.deviceId === source.id
      );
      if (existing) {
        showControllerToast(source, existing.index);
        return;
      }
      const used = new Set(window.Snowglass.input.assignments.map((assignment) => assignment.index));
      let index = 0;
      while (used.has(index)) index++;
      const next = window.Snowglass.input.assignments.map((assignment) => ({
        index: assignment.index,
        sourceId: assignment.source.kind === "keyboardMouse"
          ? "keyboard-mouse"
          : assignment.source.kind === "device" ? assignment.source.deviceId : "",
      })).filter((assignment) => assignment.sourceId);
      next.push({ index, sourceId: source.id });
      await window.Snowglass.input.assign(next);
      showControllerToast(source, index);
    }).catch((error) => {
      setModel("error", error instanceof Error ? error.message : String(error));
    });
  };
  const controllerConnected = (event: Event) => assignController((event as CustomEvent<InputSource>).detail);
  window.Snowglass.input.addEventListener("controllerconnected", controllerConnected);
  for (const controller of window.Snowglass.input.controllers) assignController(controller);

  const prepare = async (path: string, emulator: string | undefined, destination: "details" | "launch") => {
    if (window.Snowglass.emulator.activeLaunch) {
      fail(new DOMException("Close the active game before opening another one", "InvalidStateError"));
      return;
    }
    detection?.abort();
    detection = new AbortController();
    setModel("error", "");
    setModel("launcher", { kind: "detecting", name: path.split(/[\\/]/).pop() || path });
    try {
      const resolved = await resolveGame(path, emulator, detection.signal);
      if (detection.signal.aborted) return;
      setModel("package", resolved.packageValue);
      setModel("draft", resolved.draft);
      if (destination === "details") {
        setModel("launcher", { kind: "details" });
        go("/details");
      } else {
        await beginLaunch(resolved.packageValue, resolved.draft);
      }
    } catch (error) {
      if (!detection.signal.aborted) fail(error);
    }
  };

  const beginLaunch = async (packageValue: NonNullable<ShellModel["package"]>, draft: PackageConfiguration) => {
    setModel("launcher", { kind: "launching", name: draft.name });
    try {
      const request = await setupLaunch(packageValue, draft);
      setModel("request", request);
      setModel("surfaces", structuredClone(request.surfaces));
      go("/game/launching", true);
    } catch (error) {
      fail(error);
    }
  };

  const launchDraft = (draft = model.draft || undefined) => {
    if (model.package && draft) void beginLaunch(model.package, draft);
  };
  const launchRecent = (game: RecentGame) => void prepare(game.path, game.emulator, "launch");
  const showDetails = (game: RecentGame) => void prepare(game.path, game.emulator, "details");
  const cancelDetails = () => {
    detection?.abort();
    setModel("launcher", { kind: "idle" });
    setModel("package", null);
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

  const acceptStartup = (request: StartupRequest) => {
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
    localStorage.setItem(RECENT_KEY, JSON.stringify({ version: 2, entries: model.recent }));
  });
  onCleanup(() => {
    detection?.abort();
    if (toastTimer) clearTimeout(toastTimer);
    window.Snowglass.input.removeEventListener("controllerconnected", controllerConnected);
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
      <div class="horizon-shell-root">
        <div class="horizon-shell-content">{props.children}</div>
        <InputNavigation />
        <ControllerConnectionToast />
      </div>
    </ShellContext.Provider>
  );
}
