import { useEffect, useRef, useState } from "react";
import { flushSync } from "react-dom";
import type {
  BackgroundRemoteValue,
  LauncherEmulator,
  LauncherGame,
  LauncherOption,
  LauncherStartRequest,
  LauncherState,
} from "./emulator";
import { InputRemapping } from "./InputRemapping";
import "./emulator";
import "./shell.css";

type ForegroundPhase = "idle" | "launching" | "running" | "closing";
type GameAudioState = { available: boolean; volume: number; muted: boolean };
type RecentGame = {
  path: string;
  name: string;
  platform: string;
  emulator: string;
  playedAt: number;
};

const DEFAULT_AUDIO: GameAudioState = { available: false, volume: 1, muted: false };
const RECENT_GAMES_KEY = "snowglass.recentGames.v2";
const RECENT_GAMES_LIMIT = 8;

function readRecentGames(): RecentGame[] {
  try {
    const stored = JSON.parse(localStorage.getItem(RECENT_GAMES_KEY) || "null");
    if (!stored || stored.version !== 2 || !Array.isArray(stored.entries)) return [];
    return stored.entries
      .filter(
        (entry: unknown): entry is RecentGame =>
          Boolean(entry) &&
          typeof entry === "object" &&
          typeof (entry as RecentGame).path === "string" &&
          typeof (entry as RecentGame).name === "string" &&
          typeof (entry as RecentGame).platform === "string" &&
          typeof (entry as RecentGame).emulator === "string" &&
          Number.isSafeInteger((entry as RecentGame).playedAt),
      )
      .slice(0, RECENT_GAMES_LIMIT);
  } catch (error) {
    console.warn("Could not read recently played games", error);
    return [];
  }
}

function writeRecentGames(entries: RecentGame[]) {
  try {
    localStorage.setItem(
      RECENT_GAMES_KEY,
      JSON.stringify({ version: 2, entries }),
    );
  } catch (error) {
    console.warn("Could not save recently played games", error);
  }
}

function relativeTime(timestamp: number) {
  const elapsed = Date.now() - timestamp;
  if (elapsed < 60_000) return "Just now";
  if (elapsed < 3_600_000) return `${Math.floor(elapsed / 60_000)} min ago`;
  if (elapsed < 86_400_000) return `${Math.floor(elapsed / 3_600_000)} hr ago`;
  return new Intl.DateTimeFormat(undefined, { month: "short", day: "numeric" }).format(timestamp);
}

function friendlyName(value: string) {
  return value
    .replace(/^NINTENDO_/, "Nintendo ")
    .replace(/^MICROSOFT_/, "Microsoft ")
    .replace(/^SONY_/, "Sony ")
    .replace(/^SEGA_/, "Sega ")
    .replaceAll("_", " ")
    .replace(/\b\w/g, (letter) => letter.toUpperCase())
    .replace(/\bWiiu\b/g, "Wii U")
    .replace(/\bDrc\b/g, "GamePad")
    .replace(/\bGcn\b/g, "GameCube")
    .replace(/\bN64\b/g, "Nintendo 64")
    .replace(/\bPsx\b/g, "PlayStation");
}

function cloneGame(game: LauncherGame): LauncherGame {
  return JSON.parse(JSON.stringify(game)) as LauncherGame;
}

function emulatorApi(): BackgroundRemoteValue {
  return (window as unknown as { readonly Emulator: BackgroundRemoteValue }).Emulator;
}

function activeEmulatorInstance(): BackgroundRemoteValue {
  return emulatorApi().activeInstance;
}

function audioState(value: unknown): GameAudioState {
  const audio = value && typeof value === "object" ? (value as Partial<GameAudioState>) : DEFAULT_AUDIO;
  const volume = Number(audio.volume);
  return {
    available: Boolean(audio.available),
    volume: Number.isFinite(volume) ? Math.max(0, Math.min(1, volume)) : 1,
    muted: Boolean(audio.muted),
  };
}

type ViewTransitionDocument = Document & {
  startViewTransition?: (update: () => void) => { finished: Promise<unknown> };
};
let activeViewTransitions = 0;

function updateWithCrossfade(update: () => void) {
  const viewDocument = document as ViewTransitionDocument;
  if (!viewDocument.startViewTransition || window.matchMedia("(prefers-reduced-motion: reduce)").matches) {
    update();
    return;
  }
  activeViewTransitions += 1;
  document.documentElement.classList.add("snowglass-view-transition");
  const finish = () => {
    activeViewTransitions -= 1;
    if (activeViewTransitions === 0) document.documentElement.classList.remove("snowglass-view-transition");
  };
  try {
    const transition = viewDocument.startViewTransition(() => flushSync(update));
    void transition.finished.catch(() => {}).then(finish);
  } catch {
    finish();
    update();
  }
}

function isGameView(phase: ForegroundPhase) {
  return phase === "running" || phase === "closing";
}

function ControllerOutline({ layout }: { layout: string }) {
  const kind = layout.toUpperCase();
  if (kind.includes("WIIU_DRC")) {
    return (
      <svg className="controller-outline drc" viewBox="0 0 240 130" aria-hidden="true">
        <path d="M27 17C14 21 8 34 8 51v51c0 15 15 22 27 13l18-15h134l18 15c12 9 27 2 27-13V51c0-17-6-30-19-34-24-8-162-8-186 0Z" />
        <rect x="70" y="27" width="100" height="64" rx="7" />
        <circle cx="42" cy="42" r="12" /><circle cx="198" cy="42" r="12" />
        <path d="M35 75v24M23 87h24" />
        <circle cx="198" cy="82" r="4" /><circle cx="212" cy="70" r="4" />
        <circle cx="184" cy="70" r="4" /><circle cx="198" cy="58" r="4" />
        <circle cx="120" cy="108" r="5" />
      </svg>
    );
  }
  if (kind.includes("WII") && !kind.includes("CLASSIC")) {
    return (
      <svg className="controller-outline remote" viewBox="0 0 110 190" aria-hidden="true">
        <rect x="22" y="5" width="66" height="180" rx="24" />
        <circle cx="55" cy="38" r="13" /><path d="M55 27v22M44 38h22" />
        <circle cx="55" cy="77" r="8" /><circle cx="55" cy="105" r="11" />
        <circle cx="55" cy="138" r="5" /><path d="M40 160h30" />
      </svg>
    );
  }
  return (
    <svg className="controller-outline gamepad" viewBox="0 0 240 130" aria-hidden="true">
      <path d="M72 28h96c21 0 36 12 45 35l17 43c5 14-11 24-22 14l-28-26H60l-28 26c-11 10-27 0-22-14l17-43c9-23 24-35 45-35Z" />
      <path d="M58 56v30M43 71h30" />
      <circle cx="182" cy="59" r="5" /><circle cx="198" cy="73" r="5" />
      <circle cx="166" cy="73" r="5" /><circle cx="182" cy="87" r="5" />
      <circle cx="91" cy="91" r="12" /><circle cx="149" cy="91" r="12" />
      <path d="M108 57h24" />
    </svg>
  );
}

function OptionField({ option, onChange }: { option: LauncherOption; onChange(value: unknown): void }) {
  if (option.kind === "boolean") {
    return (
      <label className="option-toggle">
        <span>{option.title}</span>
        <input type="checkbox" checked={Boolean(option.value)} onChange={(event) => onChange(event.target.checked)} />
        <i aria-hidden="true" />
      </label>
    );
  }
  if (option.kind === "select") {
    return (
      <label className="config-field"><span>{option.title}</span>
        <select value={String(option.value ?? "")} onChange={(event) => onChange(event.target.value)}>
          {(option.choices || []).map((choice) => <option key={choice.key} value={choice.key}>{choice.label}</option>)}
        </select>
      </label>
    );
  }
  const numeric = option.kind === "integer" || option.kind === "float";
  return (
    <label className="config-field"><span>{option.title}</span>
      <input
        type={numeric ? "number" : "text"}
        step={option.kind === "integer" ? 1 : option.kind === "float" ? "any" : undefined}
        min={option.min}
        max={option.max}
        value={String(option.value ?? "")}
        onChange={(event) => onChange(numeric ? Number(event.target.value) : event.target.value)}
      />
    </label>
  );
}

function DropHome({
  state,
  recent,
  dragging,
  setDragging,
  onRecent,
  onClear,
}: {
  state: LauncherState;
  recent: RecentGame[];
  dragging: boolean;
  setDragging(value: boolean): void;
  onRecent(game: RecentGame): void;
  onClear(): void;
}) {
  const busy = state.kind === "detecting";
  return (
    <section className="library-home">
      <header className="library-brand"><span className="brand-gem">◇</span><div><p>Snowglass</p><h1>Your games, one drop away.</h1></div></header>
      <div
        className="drop-hero"
        data-dragging={dragging ? "true" : "false"}
        data-busy={busy ? "true" : "false"}
        onDragEnter={(event) => { event.preventDefault(); setDragging(true); }}
        onDragOver={(event) => { event.preventDefault(); event.dataTransfer.dropEffect = "copy"; setDragging(true); }}
        onDragLeave={(event) => { if (event.currentTarget === event.target) setDragging(false); }}
        onDrop={(event) => { event.preventDefault(); setDragging(false); }}
      >
        <div className="drop-orbit"><span className="drop-icon">＋</span></div>
        <div>
          <p className="drop-kicker">{busy ? "Detecting system" : dragging ? "Release to add" : "Drag & drop"}</p>
          <h2>{busy ? state.name : "Drop a game or game folder here"}</h2>
          <p>{busy ? "Finding a compatible emulator…" : "Snowglass detects the system and finds an installed emulator."}</p>
        </div>
      </div>
      {state.kind === "error" && <div className="launcher-error" role="alert"><span>!</span><p>{state.message}</p></div>}
      <section className="recent-library" aria-labelledby="recent-games-title">
        <div className="library-section-title"><div><p>Library</p><h2 id="recent-games-title">Recently played</h2></div>{recent.length > 0 && <button type="button" onClick={onClear}>Clear</button>}</div>
        {recent.length ? (
          <div className="recent-grid">
            {recent.map((game) => (
              <button className="recent-card" type="button" key={game.path} disabled={busy} onClick={() => onRecent(game)}>
                <span className="recent-art"><span>▶</span></span>
                <span className="recent-copy"><strong>{game.name}</strong><small>{friendlyName(game.platform)}</small><time>{relativeTime(game.playedAt)}</time></span>
              </button>
            ))}
          </div>
        ) : (
          <div className="recent-empty"><span>◌</span><div><strong>No recent games</strong><p>Games you start will stay within reach here.</p></div></div>
        )}
      </section>
    </section>
  );
}

function GameConfiguration({ game, onChange, onCancel, onStart }: {
  game: LauncherGame;
  onChange(game: LauncherGame): void;
  onCancel(): void;
  onStart(): void;
}) {
  const candidate = game.candidates.find(({ id }) => id === game.selectedEmulator) || game.candidates[0];
  const updateCandidate = (update: (candidate: LauncherEmulator) => void) => {
    const next = cloneGame(game);
    const selected = next.candidates.find(({ id }) => id === next.selectedEmulator) || next.candidates[0];
    update(selected);
    onChange(next);
  };
  return (
    <section className="launch-config">
      <header className="config-header">
        <button className="back-button" type="button" onClick={onCancel} aria-label="Back to library">←</button>
        <div className="game-identity"><span className="game-badge">▶</span><div><p>{friendlyName(game.platform)}</p><h1>{game.name}</h1><small title={game.path}>{game.path}</small></div></div>
        <button className="primary-start compact" type="button" onClick={onStart}>Start <span>▶</span></button>
      </header>
      <div className="config-layout">
        <main className="controller-setup">
          <div className="config-section-heading"><div><p>Input assignments</p><h2>Controllers</h2></div><span>Choose what sits in each virtual port</span></div>
          {candidate.groups.length === 0 ? <div className="no-controllers">This emulator does not expose configurable controller ports.</div> : candidate.groups.map((group, groupIndex) => (
            <section className="controller-group" key={group.name}>
              <div className="controller-group-title"><h3>{friendlyName(group.name)}</h3><label><span>Connected</span><select value={group.connected} onChange={(event) => updateCandidate((selected) => { selected.groups[groupIndex].connected = Number(event.target.value); })}>{group.ports.map((_, index) => <option key={index} value={index}>{index}</option>)}<option value={group.ports.length}>{group.ports.length}</option></select></label></div>
              <div className="controller-grid">
                {group.ports.slice(0, group.connected).map((port, portIndex) => (
                  <article className="controller-card" key={port.index}>
                    <span className="port-number">P{port.index + 1}</span>
                    <ControllerOutline layout={port.selectedLayout} />
                    <strong>{friendlyName(port.selectedLayout)}</strong>
                    <div className="controller-selects">
                      <label><span>Device</span><select value={port.selectedDevice} onChange={(event) => updateCandidate((selected) => { selected.groups[groupIndex].ports[portIndex].selectedDevice = event.target.value; })}>
                        <option value="K">Keyboard + mouse</option><option value="0">Controller 1</option><option value="1">Controller 2</option><option value="2">Controller 3</option><option value="3">Controller 4</option>
                      </select></label>
                      <label><span>Layout</span><select value={port.selectedLayout} onChange={(event) => updateCandidate((selected) => { selected.groups[groupIndex].ports[portIndex].selectedLayout = event.target.value; })}>{port.layouts.map((layout) => <option key={layout} value={layout}>{friendlyName(layout)}</option>)}</select></label>
                    </div>
                  </article>
                ))}
              </div>
            </section>
          ))}
        </main>
        <aside className="emulator-settings">
          <div className="config-section-heading"><div><p>Runtime</p><h2>Emulator</h2></div></div>
          <label className="config-field"><span>Use emulator</span><select value={game.selectedEmulator} onChange={(event) => onChange({ ...game, selectedEmulator: event.target.value })}>{game.candidates.map(({ id }) => <option key={id} value={id}>{friendlyName(id)}</option>)}</select></label>
          {candidate.options.length > 0 && <div className="option-stack">{candidate.options.map((option, index) => <OptionField key={option.path} option={option} onChange={(value) => updateCandidate((selected) => { selected.options[index].value = value; })} />)}</div>}
          {candidate.options.length === 0 && <p className="settings-empty">This emulator is ready with its default settings.</p>}
          <button className="primary-start" type="button" onClick={onStart}><span>▶</span> Start game</button>
        </aside>
      </div>
    </section>
  );
}

export function ShellApp() {
  const [launcherState, setLauncherState] = useState<LauncherState>(() => window.snowglassLauncher.state);
  const [draft, setDraft] = useState<LauncherGame | null>(() => launcherState.kind === "configure" ? cloneGame(launcherState.game) : null);
  const [recentGames, setRecentGames] = useState<RecentGame[]>(readRecentGames);
  const [dragging, setDragging] = useState(false);
  const [phase, setPhase] = useState<ForegroundPhase>("idle");
  const phaseRef = useRef<ForegroundPhase>("idle");
  const pendingRecent = useRef<RecentGame | null>(null);
  const [status, setStatus] = useState("Choose a game to start");
  const [error, setError] = useState("");
  const [audio, setAudio] = useState<GameAudioState>(DEFAULT_AUDIO);
  const [confirmClose, setConfirmClose] = useState(false);
  const [remappingOpen, setRemappingOpen] = useState(false);

  const updatePhase = (nextPhase: ForegroundPhase) => {
    const previous = phaseRef.current;
    if (previous === nextPhase) return;
    phaseRef.current = nextPhase;
    const update = () => setPhase(nextPhase);
    isGameView(previous) !== isGameView(nextPhase) ? updateWithCrossfade(update) : update();
  };

  const rememberPendingGame = () => {
    const game = pendingRecent.current;
    if (!game) return;
    pendingRecent.current = null;
    setRecentGames((entries) => [game, ...entries.filter(({ path }) => path !== game.path)].slice(0, RECENT_GAMES_LIMIT));
  };

  useEffect(() => writeRecentGames(recentGames), [recentGames]);

  useEffect(() => {
    const changed = (event: Event) => {
      const state = (event as CustomEvent<LauncherState>).detail;
      setLauncherState(state);
      if (state.kind === "configure") { setDraft(cloneGame(state.game)); setError(""); }
      if (state.kind === "detecting") setError("");
      if (state.kind === "launching") { updatePhase("launching"); setStatus(`Launching ${state.name}`); setError(""); }
      if (state.kind === "error") {
        pendingRecent.current = null;
        updatePhase("idle");
        setError(state.message);
        setStatus(state.message);
      }
    };
    window.snowglassLauncher.addEventListener("statechange", changed);
    return () => window.snowglassLauncher.removeEventListener("statechange", changed);
  }, []);

  useEffect(() => {
    if (!remappingOpen) return;
    void activeEmulatorInstance().deactivateInput().catch(() => {});
    return () => { void activeEmulatorInstance().activateInput().catch(() => {}); };
  }, [remappingOpen]);

  useEffect(() => {
    let subscribed = true;
    let observing = false;
    let observed = false;
    let retryTimer: ReturnType<typeof setTimeout> | null = null;
    let instance: BackgroundRemoteValue | null = null;
    let binding = 0;
    const updateAudio = (value: unknown) => { if (subscribed) setAudio(audioState(value)); };
    const ready = () => { if (!subscribed) return; updatePhase("running"); setStatus("Game is running"); setError(""); rememberPendingGame(); };
    const failed = (value: unknown) => {
      if (!subscribed) return;
      const message = value && typeof value === "object" && "message" in value ? String(value.message) : "Game launch failed";
      pendingRecent.current = null; updatePhase("idle"); setStatus(message); setError(message); window.snowglassLauncher.cancel();
    };
    const closed = () => {
      if (!subscribed) return;
      updatePhase("idle"); setStatus("Game closed"); setError(""); setAudio(DEFAULT_AUDIO); setConfirmClose(false); setRemappingOpen(false); window.snowglassLauncher.cancel();
    };
    const detach = (target: BackgroundRemoteValue | null) => {
      if (!target) return;
      void target.off("ready", ready).catch(() => {}); void target.off("audioChanged", updateAudio).catch(() => {}); void target.off("error", failed).catch(() => {}); void target.off("closed", closed).catch(() => {});
    };
    const bindActiveInstance = async () => {
      const currentBinding = ++binding;
      const resolved = (await activeEmulatorInstance()) as unknown;
      if (!subscribed || currentBinding !== binding) return;
      detach(instance); instance = resolved ? (resolved as BackgroundRemoteValue) : null;
      if (!instance) {
        const hadGame = phaseRef.current !== "idle";
        updatePhase("idle"); setAudio(DEFAULT_AUDIO); setConfirmClose(false); setRemappingOpen(false);
        if (hadGame) window.snowglassLauncher.cancel();
        return;
      }
      setError("");
      await Promise.all([instance.on("ready", ready), instance.on("audioChanged", updateAudio), instance.on("error", failed), instance.on("closed", closed)]);
      if (!subscribed || currentBinding !== binding) return;
      updateAudio((await instance.audio) as unknown);
      const nativeState = String(await instance.state);
      if (!subscribed || currentBinding !== binding) return;
      if (nativeState === "ready") ready();
      else if (nativeState === "closing") { updatePhase("closing"); setStatus("Closing game"); }
      else if (nativeState === "error") { const remoteError = (await instance.error) as unknown; failed({ message: remoteError ? String(await (remoteError as BackgroundRemoteValue).message) : "Game launch failed" }); }
      else if (nativeState === "closed") closed();
      else { updatePhase("launching"); setStatus("Launching game"); }
    };
    const activeChanged = () => { void bindActiveInstance().catch((cause) => console.error("Could not bind the active emulator", cause)); };
    const observeActiveInstance = () => {
      if (!subscribed || observing || observed) return;
      observing = true;
      void (async () => { await emulatorApi().addEventListener("activeinstancechange", activeChanged); observed = true; if (!subscribed) { await emulatorApi().removeEventListener("activeinstancechange", activeChanged); return; } await bindActiveInstance(); })()
        .catch((cause) => console.error("Could not observe the active emulator", cause))
        .finally(() => { observing = false; if (subscribed && !observed && window.gameLayer.ready && retryTimer === null) retryTimer = setTimeout(() => { retryTimer = null; observeActiveInstance(); }, 100); });
    };
    window.gameLayer.addEventListener("load", observeActiveInstance); if (window.gameLayer.ready) observeActiveInstance();
    return () => { subscribed = false; binding += 1; window.gameLayer.removeEventListener("load", observeActiveInstance); if (retryTimer !== null) clearTimeout(retryTimer); detach(instance); if (observed) void emulatorApi().removeEventListener("activeinstancechange", activeChanged).catch(() => {}); };
  }, []);

  const startGame = () => {
    if (!draft) return;
    const candidate = draft.candidates.find(({ id }) => id === draft.selectedEmulator) || draft.candidates[0];
    const request: LauncherStartRequest = {
      generation: draft.generation,
      emulator: candidate.id,
      groups: candidate.groups.map((group) => ({ name: group.name, ports: group.ports.slice(0, group.connected).map((port) => ({ index: port.index, layout: port.selectedLayout, device: port.selectedDevice })) })),
      settings: Object.fromEntries(candidate.options.filter(({ value }) => value !== null && value !== undefined).map(({ path, value }) => [path, value])),
    };
    pendingRecent.current = { path: draft.path, name: draft.name, platform: draft.platform, emulator: candidate.id, playedAt: Date.now() };
    window.snowglassLauncher.start(request);
  };

  const gameActive = isGameView(phase);
  const homeState: LauncherState = launcherState.kind === "idle" && error
    ? { kind: "error", message: error }
    : launcherState;
  const setGameVolume = (volume: number) => { volume = Math.max(0, Math.min(1, volume)); setAudio((state) => ({ ...state, volume })); void activeEmulatorInstance().setVolume(volume).catch((cause: unknown) => console.error("Could not set game volume", cause)); };
  const toggleGameMuted = () => { const muted = !audio.muted; setAudio((state) => ({ ...state, muted })); void activeEmulatorInstance().setMuted(muted).catch((cause: unknown) => console.error("Could not change game mute state", cause)); };
  const closeGame = () => { setConfirmClose(false); updatePhase("closing"); setStatus("Closing game"); void activeEmulatorInstance().close().catch((cause: unknown) => { updatePhase("running"); setError(cause instanceof Error ? cause.message : String(cause)); }); };

  return (
    <>
      <main className="shell" data-game-active={gameActive ? "true" : "false"} data-snowglass-foreground-hit-region>
        {launcherState.kind === "configure" && draft ? (
          <GameConfiguration game={draft} onChange={setDraft} onCancel={() => window.snowglassLauncher.cancel()} onStart={startGame} />
        ) : launcherState.kind === "launching" || (phase === "launching" && pendingRecent.current) ? (
          <section className="launching-screen"><div className="launch-spinner" /><p>Starting game</p><h1>{launcherState.kind === "launching" ? launcherState.name : pendingRecent.current?.name}</h1><small>{status}</small></section>
        ) : (
          <DropHome state={homeState} recent={recentGames} dragging={dragging} setDragging={setDragging} onRecent={(game) => window.snowglassLauncher.open(game.path)} onClear={() => setRecentGames([])} />
        )}
      </main>

      <div className="game-controls-layer" data-visible={gameActive ? "true" : "false"}>
        <div className="game-hotbar-trigger" data-snowglass-foreground-hit-region>
          <div className="game-hotbar" role="toolbar" aria-label="Game controls">
            <button className="hotbar-button hotbar-power" type="button" aria-label="Close game" title="Close game" onClick={() => setConfirmClose(true)}><svg viewBox="0 0 24 24" aria-hidden="true"><path d="M12 2.5v9" /><path d="M6.35 5.65a8 8 0 1 0 11.3 0" /></svg></button>
            <span className="hotbar-divider" />
            <button className="hotbar-button hotbar-remapping" type="button" aria-label="Controller mapping" aria-pressed={remappingOpen} title="Controller mapping" onClick={() => setRemappingOpen(true)}><svg viewBox="0 0 24 24" aria-hidden="true"><path d="M7.5 8h9a4.5 4.5 0 0 1 4.2 6.1l-1.1 2.8a2 2 0 0 1-3.2.8L14.7 16H9.3l-1.7 1.7a2 2 0 0 1-3.2-.8l-1.1-2.8A4.5 4.5 0 0 1 7.5 8Z" /><path d="M7 11v4M5 13h4" /><circle cx="16.5" cy="12" r=".7" /><circle cx="18.5" cy="14" r=".7" /></svg></button>
            <button className="hotbar-button hotbar-audio" type="button" aria-label={audio.muted ? "Unmute game" : "Mute game"} aria-pressed={audio.muted} title={audio.muted ? "Unmute" : "Mute"} onClick={toggleGameMuted}><svg viewBox="0 0 24 24" aria-hidden="true"><path d="M4 9v6h4l5 4V5L8 9H4z" className="speaker-body" />{audio.muted || audio.volume === 0 ? <><path d="m17 9 5 5" /><path d="m22 9-5 5" /></> : <><path d="M16 8.5a5 5 0 0 1 0 7" />{audio.volume > 0.55 && <path d="M18.5 6a8.5 8.5 0 0 1 0 12" />}</>}</svg></button>
            <input className="hotbar-volume" type="range" min="0" max="1" step="0.01" value={audio.volume} aria-label="Game volume" title={`Volume ${Math.round(audio.volume * 100)}%`} onChange={(event) => setGameVolume(Number(event.target.value))} />
          </div>
        </div>
      </div>

      {remappingOpen && <InputRemapping onClose={() => setRemappingOpen(false)} />}
      {confirmClose && <div className="close-confirmation" role="presentation" data-snowglass-foreground-hit-region><section className="close-confirmation-dialog" role="dialog" aria-modal="true" aria-label="Close game?"><h1>Close this game?</h1><p>Any unsaved progress will be lost.</p><div className="close-confirmation-actions"><button className="dialog-button secondary" type="button" onClick={() => setConfirmClose(false)}>Keep playing</button><button className="dialog-button danger" type="button" onClick={closeGame}>Close game</button></div></section></div>}
    </>
  );
}
