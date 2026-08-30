import { Collapsible, Dialog, Field, FloatingPanel, NumberInput, Select, Slider, Switch, Tabs, createListCollection } from "@ark-ui/solid";
import { MemoryRouter, Route, createMemoryHistory, useNavigate } from "@solidjs/router";
import {
  For,
  Show,
  createContext,
  createEffect,
  createMemo,
  createSignal,
  onCleanup,
  onMount,
  useContext,
  type JSX,
  type ParentProps,
} from "solid-js";
import { createStore, unwrap } from "solid-js/store";
import type {
  ConfigurationOption,
  ConfigurationSelection,
  ConsoleStorageVolume,
  EmulatorConfiguration,
  HTMLTrackedSurfaceElement,
  LaunchInstance,
  LaunchOptions,
  LaunchRequest,
  PackageConfiguration,
  ScrapedPackage,
} from "../../../../js/snowglass";
import { InputRemapping } from "./input-remapping";

type RecentGame = { path: string; name: string; platform: string; emulator: string; playedAt: number };
type AudioState = { available: boolean; volume: number; muted: boolean };
const RECENT_KEY = "snowglass.recentGames.v2";
const RECENT_LIMIT = 8;
const WII_PLATFORM = "NINTENDO_WII";
const WIIU_PLATFORM = "NINTENDO_WIIU";
const GAMECUBE_PLATFORM = "NINTENDO_GCN";
const DREAMCAST_PLATFORM = "SEGA_DC";
const PSX_PLATFORM = "SONY_PSX";
const PS2_PLATFORM = "SONY_PS2";
const WII_REMOTE_LAYOUT = "WII_REMOTE_CONTROLLER";
const DEFAULT_AUDIO: AudioState = { available: false, volume: 1, muted: false };
let defaultWiiUNand: Promise<ConsoleStorageVolume> | null = null;
let defaultWiiUSave: Promise<ConsoleStorageVolume> | null = null;
let defaultWiiNand: Promise<ConsoleStorageVolume> | null = null;
let defaultWiiSave: Promise<ConsoleStorageVolume> | null = null;
let defaultGameCubeCard: Promise<ConsoleStorageVolume> | null = null;
let defaultDreamcastCard: Promise<ConsoleStorageVolume> | null = null;
let defaultPs1Card: Promise<ConsoleStorageVolume> | null = null;
let defaultPs2Card: Promise<ConsoleStorageVolume> | null = null;
let memoryNavigate: ((path: string, options?: { replace?: boolean }) => void) | null = null;
type StartupRequest =
  | { kind: "package"; path: string; emulator?: string }
  | { kind: "launch"; options: LaunchRequest };
let startupHandler: ((request: StartupRequest) => void) | null = null;
const queuedStartup: StartupRequest[] = [];
window.Snowglass.scraping.addEventListener("request", (event: Event) => {
  const request = (event as CustomEvent<StartupRequest>).detail;
  if (startupHandler) startupHandler(request); else queuedStartup.push(request);
});

function navigateMemory(path: string, replace: boolean) {
  if (memoryNavigate) memoryNavigate(path, { replace });
  else shellHistory.set({ value: path, replace, scroll: false });
}

async function readRecent(): Promise<RecentGame[]> {
  try {
    const value = JSON.parse(await localStorage.getItem(RECENT_KEY) || "null");
    return value?.version === 2 && Array.isArray(value.entries) ? value.entries.slice(0, RECENT_LIMIT) : [];
  } catch { return []; }
}

function friendly(value: string) {
  return value.replace(/^NINTENDO_/, "Nintendo ").replace(/^SONY_/, "Sony ").replace(/^MICROSOFT_/, "Microsoft ")
    .replaceAll("_", " ").replace(/\b\w/g, (letter) => letter.toUpperCase()).replace(/Wiiu/g, "Wii U");
}

function cloneGame(game: PackageConfiguration) { return structuredClone(unwrap(game)); }

function applyFrontendDefaults(game: PackageConfiguration) {
  if (game.platform !== WII_PLATFORM) return game;
  for (const emulator of game.candidates) {
    for (const group of emulator.groups) {
      if (group.name === "gamecube") group.connected = 0;
      if (group.name !== "wii") continue;
      group.connected = Math.min(1, group.ports.length);
      const first = group.ports[0];
      if (first?.layouts.includes(WII_REMOTE_LAYOUT)) first.selectedLayout = WII_REMOTE_LAYOUT;
    }
  }
  return game;
}

function activeOptions(emulator: EmulatorConfiguration, platform: string) {
  const activePorts = emulator.groups.flatMap((group) =>
    group.ports.slice(0, group.connected).map((port) => `ports.${platform}.${group.name}.${port.index}.`)
  );
  return emulator.options.filter((option) =>
    !option.path.startsWith("ports.") || activePorts.some((prefix) => option.path.startsWith(prefix))
  );
}

function ensureDefaultWiiUNand() {
  if (!defaultWiiUNand) {
    const pending = window.Snowglass.consolestorage.Storage.list({ kind: "nand" }).then((nands) =>
      nands.find((nand) => nand.platform === WIIU_PLATFORM)
        ?? window.Snowglass.consolestorage.Storage.create("nand", WIIU_PLATFORM, { label: "Default" })
    );
    defaultWiiUNand = pending.catch((error) => {
      defaultWiiUNand = null;
      throw error;
    });
  }
  return defaultWiiUNand;
}

function ensureDefaultWiiUSave() {
  if (!defaultWiiUSave) {
    const pending = window.Snowglass.consolestorage.Storage.list({ kind: "saveRepo" }).then((saves) =>
      saves.find((save) => save.platform === WIIU_PLATFORM)
        ?? window.Snowglass.consolestorage.Storage.create("saveRepo", WIIU_PLATFORM, { label: "Default" })
    );
    defaultWiiUSave = pending.catch((error) => {
      defaultWiiUSave = null;
      throw error;
    });
  }
  return defaultWiiUSave;
}

function ensureDefaultWiiNand() {
  if (!defaultWiiNand) {
    const pending = window.Snowglass.consolestorage.Storage.list({ kind: "nand" }).then((nands) =>
      nands.find((nand) => nand.platform === WII_PLATFORM)
        ?? window.Snowglass.consolestorage.Storage.create("nand", WII_PLATFORM, { label: "Default" })
    );
    defaultWiiNand = pending.catch((error) => {
      defaultWiiNand = null;
      throw error;
    });
  }
  return defaultWiiNand;
}

function ensureDefaultWiiSave() {
  if (!defaultWiiSave) {
    const pending = window.Snowglass.consolestorage.Storage.list({ kind: "saveRepo" }).then((saves) =>
      saves.find((save) => save.platform === WII_PLATFORM)
        ?? window.Snowglass.consolestorage.Storage.create("saveRepo", WII_PLATFORM, { label: "Default" })
    );
    defaultWiiSave = pending.catch((error) => {
      defaultWiiSave = null;
      throw error;
    });
  }
  return defaultWiiSave;
}

function ensureDefaultPs1Card() {
  if (!defaultPs1Card) {
    const pending = window.Snowglass.consolestorage.Storage.list({ kind: "memoryCard" }).then((volumes) =>
      volumes
        .filter((volume) => volume.platform === PSX_PLATFORM && volume.label === "Default")
        .sort((left, right) => right.created.getTime() - left.created.getTime())[0]
        ?? window.Snowglass.consolestorage.Storage.create("memoryCard", PSX_PLATFORM, { label: "Default" })
    );
    defaultPs1Card = pending.catch((error) => {
      defaultPs1Card = null;
      throw error;
    });
  }
  return defaultPs1Card;
}

function ensureDefaultGameCubeCard() {
  if (!defaultGameCubeCard) {
    const pending = window.Snowglass.consolestorage.Storage.list({ kind: "memoryCard" }).then((volumes) =>
      volumes
        .filter((volume) => volume.platform === GAMECUBE_PLATFORM && volume.label === "Default")
        .sort((left, right) => right.created.getTime() - left.created.getTime())[0]
        ?? window.Snowglass.consolestorage.Storage.create("memoryCard", GAMECUBE_PLATFORM, { label: "Default" })
    );
    defaultGameCubeCard = pending.catch((error) => {
      defaultGameCubeCard = null;
      throw error;
    });
  }
  return defaultGameCubeCard;
}

function ensureDefaultDreamcastCard() {
  if (!defaultDreamcastCard) {
    const pending = window.Snowglass.consolestorage.Storage.list({ kind: "memoryCard" }).then((volumes) =>
      volumes
        .filter((volume) => volume.platform === DREAMCAST_PLATFORM && volume.label === "Default")
        .sort((left, right) => right.created.getTime() - left.created.getTime())[0]
        ?? window.Snowglass.consolestorage.Storage.create("memoryCard", DREAMCAST_PLATFORM, { label: "Default" })
    );
    defaultDreamcastCard = pending.catch((error) => {
      defaultDreamcastCard = null;
      throw error;
    });
  }
  return defaultDreamcastCard;
}

function ensureDefaultPs2Card() {
  if (!defaultPs2Card) {
    const pending = window.Snowglass.consolestorage.Storage.list({ kind: "memoryCard" }).then((volumes) =>
      volumes
        .filter((volume) => volume.platform === PS2_PLATFORM && volume.label === "Default")
        .sort((left, right) => right.created.getTime() - left.created.getTime())[0]
        ?? window.Snowglass.consolestorage.Storage.create("memoryCard", PS2_PLATFORM, { label: "Default" })
    );
    defaultPs2Card = pending.catch((error) => {
      defaultPs2Card = null;
      throw error;
    });
  }
  return defaultPs2Card;
}

type LauncherUiState =
  | { kind: "idle" }
  | { kind: "detecting"; path: string; name: string }
  | { kind: "configure" }
  | { kind: "launching"; name: string }
  | { kind: "error"; message: string };

type ShellModel = {
  launcher: LauncherUiState;
  package: ScrapedPackage | null;
  draft: PackageConfiguration | null;
  request: LaunchRequest | null;
  surfaces: LaunchRequest["surfaces"];
  launch: LaunchInstance | null;
  audio: AudioState;
  error: string;
  recent: RecentGame[];
  gamepadOpen: boolean;
};

type ShellContextValue = {
  model: ShellModel;
  setModel: (...path: any[]) => void;
  navigate(path: string): void;
  open(path: string, emulator?: string): void;
  start(): void;
  cancel(): void;
  close(): void;
};
const ShellContext = createContext<ShellContextValue>();
function useShell() {
  const value = useContext(ShellContext);
  if (!value) throw new Error("Shell context is unavailable");
  return value;
}

function ShellProvider(props: ParentProps) {
  const navigate = useNavigate();
  memoryNavigate = navigate;
  onCleanup(() => { if (memoryNavigate === navigate) memoryNavigate = null; });
  const [model, updateModel] = createStore<ShellModel>({
    launcher: { kind: "idle" },
    package: null,
    draft: null,
    request: null,
    surfaces: [],
    launch: window.Snowglass.emulator.activeLaunch,
    audio: DEFAULT_AUDIO,
    error: "",
    recent: [],
    gamepadOpen: false,
  });
  const setModel = (...path: any[]) => (updateModel as (...args: any[]) => void)(...path);
  let recentLoaded = false;
  onMount(() => {
    void readRecent().then((recent) => {
      recentLoaded = true;
      setModel("recent", recent);
    });
  });
  void ensureDefaultWiiUNand().catch(() => {});

  let detection: AbortController | null = null;

  const fail = (error: unknown) => {
    const message = error instanceof Error ? error.message : String(error);
    setModel("error", message);
    setModel("launcher", { kind: "error", message });
    navigateMemory("/", true);
  };

  const open = (path: string, emulator?: string) => {
    if (window.Snowglass.emulator.activeLaunch) {
      fail(new DOMException("Close the active game before opening another one", "InvalidStateError"));
      return;
    }
    detection?.abort();
    detection = new AbortController();
    const signal = detection.signal;
    setModel("error", "");
    setModel("launcher", { kind: "detecting", path, name: path.split(/[\\/]/).pop() || path });
    navigateMemory("/", true);
    void (async () => {
      try {
        const packageValue = await window.Snowglass.scraping.Package.fromPath(path, { signal });
        const description = await window.Snowglass.configuration.describe(packageValue, { emulator });
        if (signal.aborted) return;
        setModel("package", packageValue);
        setModel("draft", applyFrontendDefaults(cloneGame(description)));
        setModel("launcher", { kind: "configure" });
        navigateMemory("/configure", true);
      } catch (error) {
        if (!signal.aborted) fail(error);
      }
    })();
  };

  const acceptStartup = (request: StartupRequest) => {
    if (request.kind === "package") {
      open(request.path, request.emulator);
      return;
    }
    setModel("request", request.options);
    setModel("surfaces", structuredClone(request.options.surfaces));
    setModel("launcher", { kind: "launching", name: request.options.command });
    navigateMemory("/game/launching", true);
  };
  startupHandler = acceptStartup;
  for (const request of queuedStartup.splice(0)) acceptStartup(request);
  onCleanup(() => {
    detection?.abort();
    if (startupHandler === acceptStartup) startupHandler = null;
  });

  createEffect(() => {
    const entries = model.recent;
    if (recentLoaded) {
      void Promise.resolve(localStorage.setItem(RECENT_KEY, JSON.stringify({ version: 2, entries })))
        .catch(() => {});
    }
  });

  const start = () => {
    const game = model.draft;
    const packageValue = model.package;
    if (!game || !packageValue) return;
    const emulator = game.candidates.find((item) => item.id === game.selectedEmulator) || game.candidates[0];
    const selection: ConfigurationSelection = {
      emulator: emulator.id,
      groups: emulator.groups.map((group) => ({
        name: group.name,
        ports: group.ports.slice(0, group.connected).map((port) => ({
          index: port.index,
          layout: port.selectedLayout,
        })),
      })),
      settings: Object.fromEntries(activeOptions(emulator, game.platform).filter((option) => option.value != null).map((option) => [option.path, option.value])),
    };
    setModel("launcher", { kind: "launching", name: game.name });
    void (async () => {
      try {
        if (game.platform === WII_PLATFORM) {
          const [save, nand] = await Promise.all([ensureDefaultWiiSave(), ensureDefaultWiiNand()]);
          selection.storage = { nand: [save.id, nand.id] };
        } else if (game.platform === WIIU_PLATFORM) {
          const [save, nand] = await Promise.all([ensureDefaultWiiUSave(), ensureDefaultWiiUNand()]);
          selection.storage = { mlc: [save.id, nand.id] };
        } else if (game.platform === GAMECUBE_PLATFORM) {
          selection.storage = { slotA: (await ensureDefaultGameCubeCard()).id };
        } else if (game.platform === DREAMCAST_PLATFORM) {
          selection.storage = { vmu: (await ensureDefaultDreamcastCard()).id };
        } else if (game.platform === PSX_PLATFORM) {
          selection.storage = { card1: (await ensureDefaultPs1Card()).id };
        } else if (game.platform === PS2_PLATFORM) {
          selection.storage = { mem1: (await ensureDefaultPs2Card()).id };
        }
        const request = await window.Snowglass.configuration.setupLaunch(packageValue, selection);
        setModel("request", request);
        setModel("surfaces", structuredClone(request.surfaces));
        navigateMemory("/game/launching", true);
      } catch (error) {
        fail(error);
      }
    })();
  };
  const cancel = () => {
    detection?.abort();
    setModel("launcher", { kind: "idle" });
    setModel("package", null);
    setModel("draft", null);
    setModel("request", null);
    setModel("surfaces", []);
    setModel("error", "");
    navigateMemory("/", true);
  };
  const close = () => model.launch?.close();

  return <ShellContext.Provider value={{ model, setModel, navigate: (path: string) => navigateMemory(path, false), open, start, cancel, close }}>{props.children}</ShellContext.Provider>;
}

function Choice(props: { label: string; value: string; items: Array<{ value: string; label: string }>; onChange(value: string): void }) {
  const collection = createMemo(() => createListCollection({ items: props.items }));
  return (
    <Select.Root collection={collection()} value={[props.value]} onValueChange={(details: any) => props.onChange(details.value[0] || "")} positioning={{ sameWidth: true }}>
      <Select.Label class="text-xs text-snow-300">{props.label}</Select.Label>
      <Select.Control><Select.Trigger class="mt-1 flex w-full items-center justify-between rounded-lg border border-snow-300 px-3 py-2"><Select.ValueText /><Select.Indicator class="symbol-glyph">⌄</Select.Indicator></Select.Trigger></Select.Control>
      <Select.Positioner class="select-positioner"><Select.Content class="rounded-lg border border-snow-300 bg-snow-900 p-1">
        <For each={props.items}>{(item) => <Select.Item item={item} class="cursor-pointer rounded px-3 py-2 data-[highlighted]:bg-snow-800"><Select.ItemText>{item.label}</Select.ItemText><Select.ItemIndicator class="symbol-glyph">✓</Select.ItemIndicator></Select.Item>}</For>
      </Select.Content></Select.Positioner>
    </Select.Root>
  );
}

function OptionControl(props: { option: ConfigurationOption; onChange(value: unknown): void }) {
  return (
    <Show when={props.option.kind !== "boolean"} fallback={
      <Switch.Root checked={Boolean(props.option.value)} onCheckedChange={(details: any) => props.onChange(details.checked)} class="flex items-center justify-between py-2">
        <Switch.Label>{props.option.title}</Switch.Label><Switch.Control class="h-6 w-11 rounded-full bg-snow-800 data-[state=checked]:bg-ice-500"><Switch.Thumb class="block h-5 w-5 translate-x-0.5 rounded-full bg-snow-100 data-[state=checked]:translate-x-5" /></Switch.Control>
      </Switch.Root>
    }>
      <Show when={props.option.kind === "select"} fallback={
        <Show when={props.option.kind === "integer" || props.option.kind === "float"} fallback={
          <Field.Root><Field.Label class="text-xs text-snow-300">{props.option.title}</Field.Label><Field.Input class="mt-1 w-full rounded-lg border border-snow-300 bg-snow-900 px-3 py-2" value={String(props.option.value ?? "")} onInput={(event: InputEvent & { currentTarget: HTMLInputElement }) => props.onChange(event.currentTarget.value)} /></Field.Root>
        }>
          <NumberInput.Root value={String(props.option.value ?? 0)} min={props.option.min} max={props.option.max} onValueChange={(details: any) => props.onChange(details.valueAsNumber)}>
            <NumberInput.Label class="text-xs text-snow-300">{props.option.title}</NumberInput.Label><NumberInput.Control class="mt-1 flex"><NumberInput.Input class="min-w-0 flex-1 rounded-l-lg border border-snow-300 bg-snow-900 px-3 py-2" /><NumberInput.DecrementTrigger class="symbol-glyph border border-snow-300 px-2">−</NumberInput.DecrementTrigger><NumberInput.IncrementTrigger class="rounded-r-lg border border-snow-300 px-2">+</NumberInput.IncrementTrigger></NumberInput.Control>
          </NumberInput.Root>
        </Show>
      }>
        <Choice label={props.option.title} value={String(props.option.value ?? "")} items={(props.option.choices || []).map((item) => ({ value: item.key, label: item.label }))} onChange={props.onChange} />
      </Show>
    </Show>
  );
}

function LibraryPage() {
  const shell = useShell();
  const [dropActive, setDropActive] = createSignal(window.Snowglass.scraping.dropActive);
  onMount(() => {
    const changed = (event: Event) => setDropActive((event as CustomEvent<boolean>).detail);
    window.Snowglass.scraping.addEventListener("dropactivechange", changed);
    onCleanup(() => window.Snowglass.scraping.removeEventListener("dropactivechange", changed));
  });
  return <main class="min-h-screen bg-snow-950 p-10 text-snow-100">
    <header class="mb-8"><p class="text-ice-300"><span class="symbol-glyph">◇</span> Snowglass</p><h1 class="text-4xl font-semibold">Your games, one drop away.</h1></header>
    <section class="rounded-2xl border-2 border-dashed border-snow-300 p-16 text-center data-[active=true]:border-ice-500 data-[active=true]:bg-snow-900" data-active={dropActive()}>
      <p class="symbol-glyph text-4xl">＋</p><h2 class="mt-3 text-2xl">{shell.model.launcher.kind === "detecting" ? `Detecting ${shell.model.launcher.name}` : dropActive() ? "Release to add" : "Drop a game or game folder here"}</h2>
    </section>
    <Show when={shell.model.error}><p role="alert" class="mt-5 rounded-lg bg-error-500 p-3">{shell.model.error}</p></Show>
    <section class="mt-10"><div class="flex justify-between"><h2 class="text-2xl">Recently played</h2><Show when={shell.model.recent.length}><button onClick={() => shell.setModel("recent", [])}>Clear</button></Show></div>
      <div class="mt-4 grid grid-cols-4 gap-4"><For each={shell.model.recent}>{(game) => <button class="rounded-xl bg-snow-900 p-4 text-left" onClick={() => shell.open(game.path, game.emulator)}><strong>{game.name}</strong><small class="block text-snow-300">{friendly(game.platform)}</small></button>}</For></div>
    </section>
  </main>;
}

function ConfigurePage() {
  const shell = useShell();
  const game = () => shell.model.draft;
  const emulator = () => game()?.candidates.find((item) => item.id === game()?.selectedEmulator) || game()?.candidates[0];
  const updateEmulator = (update: (target: EmulatorConfiguration) => void) => {
    const next = cloneGame(game()!); const target = next.candidates.find((item) => item.id === next.selectedEmulator) || next.candidates[0]; update(target); shell.setModel("draft", next);
  };
  return <Show when={game()}>{(current) => <main class="configuration-page">
    <header class="configuration-header">
      <button class="configuration-back symbol-glyph" onClick={shell.cancel} aria-label="Return to library">←</button>
      <div class="configuration-title"><p>{friendly(current().platform)}</p><h1>{current().name}</h1></div>
      <button class="configuration-start" onClick={shell.start}>Start <span class="symbol-glyph" aria-hidden="true">▶</span></button>
    </header>
    <div class="configuration-layout">
      <section class="controller-pane">
        <div class="configuration-section-heading"><div><p>Emulator input</p><h2>Controller ports</h2></div><span>Choose the emulated layout for each port</span></div>
        <For each={emulator()?.groups}>{(group, groupIndex) => <section class="controller-group-card">
          <div class="controller-group-heading"><h3>{friendly(group.name)}</h3><div class="connected-choice"><Choice label="Connected" value={String(group.connected)} items={Array.from({ length: group.ports.length + 1 }, (_, value) => ({ value: String(value), label: String(value) }))} onChange={(value) => updateEmulator((target) => target.groups[groupIndex()].connected = Number(value))} /></div></div>
          <Show when={group.connected > 0} fallback={<p class="controller-empty">No controllers connected to this group.</p>}>
            <Tabs.Root defaultValue={`p${group.ports[0]?.index ?? 0}`}>
              <Tabs.List class="controller-tabs"><For each={group.ports.slice(0, group.connected)}>{(port) => <Tabs.Trigger value={`p${port.index}`} class="controller-tab">Port {port.index + 1}</Tabs.Trigger>}</For></Tabs.List>
              <For each={group.ports.slice(0, group.connected)}>{(port, portIndex) => <Tabs.Content value={`p${port.index}`} class="controller-tab-content"><article class="controller-port-card"><strong>Port {port.index + 1}</strong><div class="controller-port-fields"><Choice label="Layout" value={port.selectedLayout} items={port.layouts.map((value) => ({ value, label: friendly(value) }))} onChange={(value) => updateEmulator((target) => target.groups[groupIndex()].ports[portIndex()].selectedLayout = value)} /></div></article></Tabs.Content>}</For>
            </Tabs.Root>
          </Show>
        </section>}</For>
      </section>
      <aside class="settings-pane">
        <Choice label="Emulator" value={current().selectedEmulator} items={current().candidates.map((item) => ({ value: item.id, label: friendly(item.id) }))} onChange={(value) => shell.setModel("draft", "selectedEmulator", value)} />
        <Collapsible.Root><Collapsible.Trigger class="settings-trigger">Settings <Collapsible.Indicator class="symbol-glyph">⌄</Collapsible.Indicator></Collapsible.Trigger><Collapsible.Content class="settings-content"><For each={activeOptions(emulator()!, current().platform)}>{(option) => <OptionControl option={option} onChange={(value) => updateEmulator((target) => { const targetOption = target.options.find((candidate) => candidate.path === option.path); if (targetOption) targetOption.value = value; })} />}</For></Collapsible.Content></Collapsible.Root>
      </aside>
    </div>
  </main>}</Show>;
}

function ClassicSurfaceBackdrop(props: { surface?: () => HTMLElement | undefined; revision?: number }) {
  let backdrop!: HTMLDivElement;
  let pendingFrame = 0;

  const updateSurfaceInset = () => {
    const surface = props.surface?.() as HTMLTrackedSurfaceElement | undefined;
    const preferred = surface?.preferredSize;
    const bounds = backdrop.getBoundingClientRect();
    if (!preferred?.width || !preferred.height || !bounds.width || !bounds.height) {
      backdrop.style.removeProperty("--surface-side-inset");
      return;
    }
    const fittedWidth = Math.min(bounds.width, bounds.height * preferred.width / preferred.height);
    const sideInset = Math.max(0, (bounds.width - fittedWidth) / 2);
    backdrop.style.setProperty("--surface-side-inset", `${sideInset}px`);
  };
  const scheduleUpdate = () => {
    if (pendingFrame) cancelAnimationFrame(pendingFrame);
    pendingFrame = requestAnimationFrame(() => {
      pendingFrame = 0;
      updateSurfaceInset();
    });
  };

  createEffect(() => {
    props.revision;
    props.surface?.();
    scheduleUpdate();
  });
  onMount(() => {
    const observer = new ResizeObserver(scheduleUpdate);
    observer.observe(backdrop);
    scheduleUpdate();
    onCleanup(() => {
      observer.disconnect();
      if (pendingFrame) cancelAnimationFrame(pendingFrame);
    });
  });

  return <div ref={backdrop} class="classic-surface-backdrop" aria-hidden="true" />;
}

function GameLayout(props: ParentProps) {
  const shell = useShell();
  const [gamepadPosition, setGamepadPosition] = createSignal({ x: 300, y: 100 });
  const [gamepadSize, setGamepadSize] = createSignal({ width: 520, height: 340 });
  const [surfaceRevision, setSurfaceRevision] = createSignal(0);
  let stage!: HTMLElement;
  const surfaceElements = new Map<number, HTMLElement>();
  const displayedSurfaces = createMemo(() => {
    const requested = shell.model.surfaces.length
      ? shell.model.surfaces
      : [{ index: 0, rotation: 0, webrtc: false }];
    if (requested.length > 1) return requested;
    const secondaryIndex = requested[0]?.index === 1 ? 0 : 1;
    return [...requested, { index: secondaryIndex, rotation: 0, webrtc: false }];
  });
  const registerSurface = (index: number, element: HTMLElement) => {
    element.setAttribute("src", String(index));
    element.setAttribute("data-index", String(index));
    surfaceElements.set(index, element);
  };
  let launchSequence = 0;
  let unbindInstance = () => {};
  onCleanup(() => { launchSequence++; unbindInstance(); });
  onMount(() => {
    let shortcutHeld = false;
    const handleGamepadShortcut = (event: KeyboardEvent) => {
      const isEnter = event.code === "Enter" || event.code === "NumpadEnter" || event.key === "Enter";
      if (event.type === "keydown" && event.altKey && isEnter) {
        event.preventDefault();
        if (!shortcutHeld && !event.repeat) shell.setModel("gamepadOpen", (open: boolean) => !open);
        shortcutHeld = true;
      } else if (event.type === "keyup" && isEnter && shortcutHeld) {
        event.preventDefault();
        shortcutHeld = false;
      }
    };
    window.addEventListener("keydown", handleGamepadShortcut);
    window.addEventListener("keyup", handleGamepadShortcut);
    onCleanup(() => {
      window.removeEventListener("keydown", handleGamepadShortcut);
      window.removeEventListener("keyup", handleGamepadShortcut);
    });
  });
  const bindInstance = (instance: LaunchInstance) => {
    unbindInstance();
    void instance.ready.catch(() => {});
    const ready = () => {
      shell.setModel("error", "");
      const game = shell.model.draft;
      if (game) {
        const emulator = game.selectedEmulator || game.candidates[0]?.id || "";
        const recent: RecentGame = { path: game.path, name: game.name, platform: game.platform, emulator, playedAt: Date.now() };
        shell.setModel("recent", (entries: RecentGame[]) => [recent, ...entries.filter((entry) => entry.path !== recent.path || entry.emulator !== recent.emulator)].slice(0, RECENT_LIMIT));
      }
      shell.navigate("/game/play");
    };
    const audio = (event: Event) => shell.setModel("audio", (event as CustomEvent<AudioState>).detail);
    const surfacesChanged = () => setSurfaceRevision((revision) => revision + 1);
    const failed = (event: Event) => { const error = (event as CustomEvent<Error>).detail; shell.setModel("error", error?.message || "Game launch failed"); shell.setModel("surfaces", []); shell.navigate("/"); };
    const closed = () => { shell.setModel("launch", null); shell.setModel("request", null); shell.setModel("audio", { ...DEFAULT_AUDIO }); shell.setModel("surfaces", []); shell.setModel("launcher", { kind: "idle" }); shell.navigate("/"); };
    instance.addEventListener("ready", ready); instance.addEventListener("audioChanged", audio); instance.addEventListener("surfacesChanged", surfacesChanged); instance.addEventListener("error", failed); instance.addEventListener("closed", closed);
    unbindInstance = () => { instance.removeEventListener("ready", ready); instance.removeEventListener("audioChanged", audio); instance.removeEventListener("surfacesChanged", surfacesChanged); instance.removeEventListener("error", failed); instance.removeEventListener("closed", closed); };
    if (instance.state === "ready") ready();
    else if (instance.state === "error") failed(new CustomEvent("error", { detail: instance.error }));
    else if (instance.state === "closed") closed();
  };
  createEffect(() => {
    const request = shell.model.request;
    const sequence = ++launchSequence;
    if (!request || shell.model.launch) return;
    const waitForConnectedLayout = (framesRemaining: number) => requestAnimationFrame(() => {
      if (sequence !== launchSequence || shell.model.request !== request) return;
      if (!stage?.isConnected) {
        if (framesRemaining > 0) waitForConnectedLayout(framesRemaining - 1);
        else { shell.setModel("error", "The game layout did not become connected"); shell.navigate("/"); }
        return;
      }
      try {
        let appendedSurface = false;
        const surfaces = request.surfaces.map((surface) => {
          let attach = surfaceElements.get(surface.index);
          if (!(attach instanceof HTMLElement) || attach.localName !== "tracked-surface") throw new Error(`Surface ${surface.index} did not mount`);
          attach.setAttribute("src", String(surface.index));
          attach.setAttribute("data-index", String(surface.index));
          if (!attach.isConnected) {
            attach = document.createElement("tracked-surface");
            attach.setAttribute("src", String(surface.index));
            attach.setAttribute("data-index", String(surface.index));
            attach.className = "absolute inset-0 h-full w-full";
            stage.prepend(attach);
            surfaceElements.set(surface.index, attach);
            appendedSurface = true;
          }
          return { ...surface, attach };
        });
        const launch = () => {
          if (sequence !== launchSequence || shell.model.request !== request) return;
          try {
            const instance = window.Snowglass.emulator.launch({ ...unwrap(request), surfaces } as LaunchOptions);
            shell.setModel("launch", instance); bindInstance(instance);
          } catch (error) { shell.setModel("error", error instanceof Error ? error.message : String(error)); shell.navigate("/"); }
        };
        if (appendedSurface) requestAnimationFrame(launch); else launch();
      } catch (error) { shell.setModel("error", error instanceof Error ? error.message : String(error)); shell.navigate("/"); }
    });
    waitForConnectedLayout(180);
  });
  return <main ref={stage} class="relative h-screen w-screen overflow-hidden bg-black text-snow-100">
    <For each={displayedSurfaces()}>{(surface, index) => <Show when={index() > 0} fallback={
      <><ClassicSurfaceBackdrop surface={() => surfaceElements.get(surface.index)} revision={surfaceRevision()} /><tracked-surface ref={(element) => registerSurface(surface.index, element)} src={String(surface.index)} data-index={String(surface.index)} class="game-surface absolute inset-0 h-full w-full" /></>
    }>
      <FloatingPanel.Root open={shell.model.gamepadOpen} onOpenChange={(details: any) => shell.setModel("gamepadOpen", details.open)} position={gamepadPosition()} onPositionChange={(details: any) => setGamepadPosition(details.position)} size={gamepadSize()} onSizeChange={(details: any) => setGamepadSize(details.size)} minSize={{ width: 240, height: 180 }} lockAspectRatio persistRect closeOnEscape={false}>
        <FloatingPanel.Positioner class="gamepad-panel-positioner" data-open={shell.model.gamepadOpen} style={`position: fixed; top: 0px; left: 0px; width: ${gamepadSize().width}px; height: ${gamepadSize().height}px; transform: translate(${gamepadPosition().x}px, ${gamepadPosition().y}px);`}>
          <FloatingPanel.Content class="gamepad-panel-content" style="width: 100%; height: 100%;">
            <FloatingPanel.Header class="gamepad-panel-header">
              <FloatingPanel.DragTrigger
                class="gamepad-panel-drag"
                onDblClick={(event: MouseEvent) => event.preventDefault()}
              >Gamepad display</FloatingPanel.DragTrigger>
              <FloatingPanel.CloseTrigger class="gamepad-panel-close symbol-glyph" data-no-drag aria-label="Hide gamepad">×</FloatingPanel.CloseTrigger>
            </FloatingPanel.Header>
            <FloatingPanel.Body class="gamepad-panel-body">
              <ClassicSurfaceBackdrop surface={() => surfaceElements.get(surface.index)} revision={surfaceRevision()} />
              <tracked-surface ref={(element) => registerSurface(surface.index, element)} src={String(surface.index)} data-index={String(surface.index)} class="game-surface absolute inset-0 h-full w-full" />
            </FloatingPanel.Body>
            <FloatingPanel.ResizeTrigger
              axis="se"
              class="gamepad-panel-resize"
              data-no-drag
              aria-label="Resize gamepad display"
            >
              <span class="gamepad-panel-resize-grip" aria-hidden="true" />
            </FloatingPanel.ResizeTrigger>
          </FloatingPanel.Content>
        </FloatingPanel.Positioner>
      </FloatingPanel.Root>
    </Show>}</For>
    {props.children}
  </main>;
}

function LaunchingPage() {
  return <div class="absolute inset-0 grid place-items-center bg-snow-950/80"><div class="w-72"><p>Launching game…</p><div role="progressbar" aria-label="Launching game" class="mt-3 h-2 overflow-hidden rounded bg-snow-800"><div class="h-full w-1/2 bg-ice-500" /></div></div></div>;
}

function PlayPage() {
  const shell = useShell(); const [confirm, setConfirm] = createSignal(false);
  const toggleMuted = () => shell.setModel("audio", "muted", (value: boolean) => { shell.model.launch?.setMuted(!value); return !value; });
  return <>
    <div class="game-controls-layer">
      <div class="game-hotbar-trigger">
        <Show when={confirm()} fallback={<div class="game-hotbar" role="toolbar" aria-label="Game controls">
          <button class="hotbar-button hotbar-power" type="button" aria-label="Close game" title="Close game" onClick={() => setConfirm(true)}><svg viewBox="0 0 24 24" aria-hidden="true"><path fill="#ffffff" d="M11 2h2v10h-2V2Zm6.83 3.17-1.42 1.42A6.96 6.96 0 0 1 19 12a7 7 0 1 1-11.42-5.42L6.17 5.17A9 9 0 1 0 21 12c0-2.74-1.23-5.18-3.17-6.83Z" /></svg></button>
          <span class="hotbar-divider" />
          <button class="hotbar-button" type="button" aria-label="Controller mapping" title="Controller mapping" onClick={() => shell.navigate("/game/remap")}><svg viewBox="0 0 24 24" aria-hidden="true"><path fill="#ffffff" d="M4 5h16a2 2 0 0 1 2 2v10a2 2 0 0 1-2 2H4a2 2 0 0 1-2-2V7a2 2 0 0 1 2-2Zm1 3v2h2V8H5Zm3 0v2h2V8H8Zm3 0v2h2V8h-2Zm3 0v2h2V8h-2Zm3 0v2h2V8h-2ZM5 12v2h2v-2H5Zm3 0v2h8v-2H8Zm9 0v2h2v-2h-2Zm-9 3v2h8v-2H8Z" /></svg></button>
          <button class="hotbar-button" type="button" aria-label={shell.model.gamepadOpen ? "Hide gamepad" : "Show gamepad"} aria-pressed={shell.model.gamepadOpen} title={shell.model.gamepadOpen ? "Hide gamepad" : "Show gamepad"} onClick={() => shell.setModel("gamepadOpen", (value: boolean) => !value)}><svg viewBox="0 0 24 24" aria-hidden="true"><path fill="#ffffff" d="M7.5 7h9a5.5 5.5 0 0 1 5.14 3.54l1.15 3.02a4 4 0 0 1-6.39 4.35L14.35 16h-4.7L7.6 17.91a4 4 0 0 1-6.39-4.35l1.15-3.02A5.5 5.5 0 0 1 7.5 7Z" /><path fill="#222326" d="M7 10h2v2h2v2H9v2H7v-2H5v-2h2v-2Zm9.5 1a1.25 1.25 0 1 1 0 2.5 1.25 1.25 0 0 1 0-2.5Zm3 2.5a1.25 1.25 0 1 1 0 2.5 1.25 1.25 0 0 1 0-2.5Z" /></svg></button>
          <span class="hotbar-divider" />
          <button class="hotbar-button hotbar-audio" type="button" aria-label={shell.model.audio.muted ? "Unmute game" : "Mute game"} aria-pressed={shell.model.audio.muted} title={shell.model.audio.muted ? "Unmute" : "Mute"} onClick={toggleMuted}><svg viewBox="0 0 24 24" aria-hidden="true"><path fill="#ffffff" d="M3 9v6h4l5 5V4L7 9H3Z" class="speaker-body" /><Show when={shell.model.audio.muted || shell.model.audio.volume === 0} fallback={<><path fill="#ffffff" d="M14.5 8.1v2.05a2.5 2.5 0 0 1 0 3.7v2.05a4.5 4.5 0 0 0 0-7.8Z" /><Show when={shell.model.audio.volume > 0.55}><path fill="#ffffff" d="M17 5.55v2.18a6 6 0 0 1 0 8.54v2.18A8 8 0 0 0 17 5.55Z" /></Show></>}><path fill="#ffffff" d="m15.3 8 2.7 2.7L20.7 8 22 9.3 19.3 12l2.7 2.7-1.3 1.3-2.7-2.7-2.7 2.7-1.3-1.3 2.7-2.7L14 9.3 15.3 8Z" /></Show></svg></button>
          <Slider.Root min={0} max={1} step={0.01} value={[shell.model.audio.volume]} onValueChange={(details: any) => { const value = details.value[0]; shell.setModel("audio", "volume", value); shell.model.launch?.setVolume(value); }} class="hotbar-volume"><Slider.Control class="hotbar-volume-control"><Slider.Track class="hotbar-volume-track"><Slider.Range class="hotbar-volume-range" /></Slider.Track><Slider.Thumb index={0} class="hotbar-volume-thumb" /></Slider.Control></Slider.Root>
        </div>}>
          <Dialog.Root open onOpenChange={(details: any) => setConfirm(details.open)} modal={false} preventScroll={false}>
            <Dialog.Content class="game-hotbar close-confirm-hotbar">
              <Dialog.Title class="close-confirm-title">Close this game?</Dialog.Title>
              <Dialog.CloseTrigger class="close-confirm-button">Keep playing</Dialog.CloseTrigger>
              <button class="close-confirm-button close-confirm-danger" onClick={shell.close}>Close game</button>
            </Dialog.Content>
          </Dialog.Root>
        </Show>
      </div>
    </div>
  </>;
}

function RemapPage() {
  const shell = useShell();
  onMount(() => { shell.model.launch?.deactivateInput(); onCleanup(() => shell.model.launch?.activateInput()); });
  return <Dialog.Root open modal={false} closeOnEscape={false}><Dialog.Positioner class="remap-positioner absolute inset-y-0 right-0 w-[34rem]"><Dialog.Content class="remap-dialog-content bg-snow-900 p-6"><Dialog.Title class="text-2xl">Controller remapping</Dialog.Title><Dialog.Description class="mt-2 text-snow-300">Choose a control, then press or move its replacement.</Dialog.Description><InputRemapping onClose={() => shell.navigate("/game/play")} /></Dialog.Content></Dialog.Positioner></Dialog.Root>;
}

function NotFound() { navigateMemory("/", true); return null; }

const shellHistory = createMemoryHistory();
shellHistory.set({ value: "/", replace: true, scroll: false });

export function ShellRouter() {
  return <MemoryRouter history={shellHistory} root={(props) => <ShellProvider>{props.children}</ShellProvider>}>
    <Route path="/" component={LibraryPage} />
    <Route path="/configure" component={ConfigurePage} />
    <Route path="/game" component={GameLayout}>
      <Route path="/launching" component={LaunchingPage} />
      <Route path="/play" component={PlayPage} />
      <Route path="/remap" component={RemapPage} />
    </Route>
    <Route path="*404" component={NotFound} />
  </MemoryRouter>;
}

declare module "solid-js" {
  namespace JSX {
    interface IntrinsicElements {
      "tracked-surface": JSX.HTMLAttributes<HTMLElement> & { src?: string };
    }
  }
}
