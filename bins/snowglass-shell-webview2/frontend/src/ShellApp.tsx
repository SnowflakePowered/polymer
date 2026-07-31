import { useEffect, useRef, useState, type FormEvent } from "react";
import { flushSync } from "react-dom";
import type {
  BackgroundRemoteValue,
  GamePadMapping,
  GraphicsHook,
  LaunchRequest,
} from "./emulator";
import { InputRemapping } from "./InputRemapping";
import "./emulator";
import "./shell.css";

type InputMapping = "none" | "controller" | "keyboard" | "keyboard-mouse";
type ForegroundPhase = "idle" | "launching" | "running" | "closing";
type GameAudioState = {
  available: boolean;
  volume: number;
  muted: boolean;
};
type LaunchHistoryEntry = {
  command: string;
  arguments: string[];
  isolated: boolean;
  hooks: GraphicsHook[];
  inputMapping: InputMapping;
  launchedAt: number;
};
type LaunchConfiguration = Omit<LaunchHistoryEntry, "launchedAt">;

const DEFAULT_AUDIO: GameAudioState = {
  available: false,
  volume: 1,
  muted: false,
};

const HOOKS: Array<{ id: GraphicsHook; label: string }> = [
  { id: "vulkan", label: "Vulkan" },
  { id: "opengl", label: "OpenGL" },
  { id: "d3d12", label: "Direct3D 12" },
  { id: "d3d11", label: "Direct3D 11" },
];
const INPUT_MAPPING_LABELS: Record<InputMapping, string> = {
  none: "No input",
  controller: "Controller 1",
  keyboard: "Keyboard",
  "keyboard-mouse": "Keyboard + mouse",
};
const LAUNCH_HISTORY_KEY = "snowglass.launchHistory.v1";
const LAUNCH_HISTORY_LIMIT = 10;

function isGraphicsHook(value: unknown): value is GraphicsHook {
  return HOOKS.some(({ id }) => id === value);
}

function isInputMapping(value: unknown): value is InputMapping {
  return (
    value === "none" ||
    value === "controller" ||
    value === "keyboard" ||
    value === "keyboard-mouse"
  );
}

function readLaunchHistory(): LaunchHistoryEntry[] {
  try {
    const stored = JSON.parse(localStorage.getItem(LAUNCH_HISTORY_KEY) || "null");
    if (
      !stored ||
      stored.version !== 1 ||
      !Array.isArray(stored.entries)
    ) {
      return [];
    }
    return stored.entries
      .filter(
        (entry: unknown): entry is LaunchHistoryEntry =>
          Boolean(entry) &&
          typeof entry === "object" &&
          typeof (entry as LaunchHistoryEntry).command === "string" &&
          Boolean((entry as LaunchHistoryEntry).command.trim()) &&
          Array.isArray((entry as LaunchHistoryEntry).arguments) &&
          (entry as LaunchHistoryEntry).arguments.every(
            (argument) => typeof argument === "string",
          ) &&
          typeof (entry as LaunchHistoryEntry).isolated === "boolean" &&
          Array.isArray((entry as LaunchHistoryEntry).hooks) &&
          (entry as LaunchHistoryEntry).hooks.length > 0 &&
          (entry as LaunchHistoryEntry).hooks.every(isGraphicsHook) &&
          isInputMapping((entry as LaunchHistoryEntry).inputMapping) &&
          Number.isSafeInteger((entry as LaunchHistoryEntry).launchedAt) &&
          (entry as LaunchHistoryEntry).launchedAt >= 0 &&
          (entry as LaunchHistoryEntry).launchedAt <= 8_640_000_000_000_000,
      )
      .slice(0, LAUNCH_HISTORY_LIMIT);
  } catch (historyError) {
    console.warn("Could not read launch history", historyError);
    return [];
  }
}

function writeLaunchHistory(entries: LaunchHistoryEntry[]) {
  try {
    localStorage.setItem(
      LAUNCH_HISTORY_KEY,
      JSON.stringify({ version: 1, entries }),
    );
  } catch (historyError) {
    console.warn("Could not save launch history", historyError);
  }
}

function launchIdentity(entry: LaunchConfiguration) {
  return JSON.stringify([
    entry.command.toLowerCase(),
    entry.arguments,
    entry.isolated,
    [...entry.hooks].sort(),
    entry.inputMapping,
  ]);
}

function commandName(command: string) {
  return command.split(/[\\/]/).filter(Boolean).pop() || command;
}

function hooksLabel(hooks: GraphicsHook[]) {
  return hooks
    .map((hook) => HOOKS.find(({ id }) => id === hook)?.label || hook)
    .join(" · ");
}

function formatArguments(arguments_: string[]) {
  return arguments_
    .map((argument) => (/\s/.test(argument) ? `"${argument}"` : argument))
    .join(" ");
}

function launchedLabel(timestamp: number) {
  const elapsed = Date.now() - timestamp;
  if (elapsed < 60_000) return "Just now";
  if (elapsed < 3_600_000) return `${Math.floor(elapsed / 60_000)} min ago`;
  if (elapsed < 86_400_000) {
    const hours = Math.floor(elapsed / 3_600_000);
    return `${hours} hr${hours === 1 ? "" : "s"} ago`;
  }
  return new Intl.DateTimeFormat(undefined, {
    month: "short",
    day: "numeric",
  }).format(timestamp);
}

function splitArguments(value: string) {
  const arguments_: string[] = [];
  let current = "";
  let quoted = false;
  for (const character of value) {
    if (character === '"') {
      quoted = !quoted;
    } else if (/\s/.test(character) && !quoted) {
      if (current) {
        arguments_.push(current);
        current = "";
      }
    } else {
      current += character;
    }
  }
  if (quoted) throw new Error("Arguments contain an unmatched quote");
  if (current) arguments_.push(current);
  return arguments_;
}

function gamepadMapping(choice: InputMapping): GamePadMapping[] {
  if (choice === "none") return [];
  return [
    {
      gamepadId: choice === "controller" ? 0 : "K",
      mouse: choice === "keyboard-mouse",
      slot: 0,
      layout: "XBOX_CONTROLLER",
      mapping: {},
    },
  ];
}

function backgroundWindow(): BackgroundRemoteValue {
  return window.gameLayer.contentWindow;
}

function emulatorApi(): BackgroundRemoteValue {
  return (window as unknown as { readonly Emulator: BackgroundRemoteValue })
    .Emulator;
}

function activeEmulatorInstance(): BackgroundRemoteValue {
  return emulatorApi().activeInstance;
}

function audioState(value: unknown): GameAudioState {
  const audio =
    value && typeof value === "object"
      ? (value as Partial<GameAudioState>)
      : DEFAULT_AUDIO;
  const volume = Number(audio.volume);
  return {
    available: Boolean(audio.available),
    volume: Number.isFinite(volume) ? Math.max(0, Math.min(1, volume)) : 1,
    muted: Boolean(audio.muted),
  };
}

type ViewTransitionDocument = Document & {
  startViewTransition?: (update: () => void) => {
    finished: Promise<unknown>;
  };
};

let activeViewTransitions = 0;

function updateWithCrossfade(update: () => void) {
  const viewDocument = document as ViewTransitionDocument;
  if (
    !viewDocument.startViewTransition ||
    window.matchMedia("(prefers-reduced-motion: reduce)").matches
  ) {
    update();
    return;
  }
  activeViewTransitions += 1;
  document.documentElement.classList.add("snowglass-view-transition");
  const finish = () => {
    activeViewTransitions -= 1;
    if (activeViewTransitions === 0) {
      document.documentElement.classList.remove(
        "snowglass-view-transition",
      );
    }
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

export function ShellApp() {
  const [command, setCommand] = useState("");
  const [argumentsText, setArgumentsText] = useState("");
  const [isolated, setIsolated] = useState(false);
  const [inputMapping, setInputMapping] =
    useState<InputMapping>("controller");
  const [hooks, setHooks] = useState<GraphicsHook[]>(
    HOOKS.map(({ id }) => id),
  );
  const [phase, setPhase] = useState<ForegroundPhase>("idle");
  const phaseRef = useRef<ForegroundPhase>("idle");
  const [status, setStatus] = useState("Choose a game to start");
  const [error, setError] = useState("");
  const [audio, setAudio] = useState<GameAudioState>(DEFAULT_AUDIO);
  const [confirmClose, setConfirmClose] = useState(false);
  const [remappingOpen, setRemappingOpen] = useState(false);
  const [launchHistory, setLaunchHistory] =
    useState<LaunchHistoryEntry[]>(readLaunchHistory);

  const updatePhase = (nextPhase: ForegroundPhase) => {
    const previousPhase = phaseRef.current;
    if (previousPhase === nextPhase) return;
    phaseRef.current = nextPhase;
    const update = () => setPhase(nextPhase);
    if (isGameView(previousPhase) !== isGameView(nextPhase)) {
      updateWithCrossfade(update);
    } else {
      update();
    }
  };

  useEffect(() => {
    writeLaunchHistory(launchHistory);
  }, [launchHistory]);

  useEffect(() => {
    if (!remappingOpen) return;
    void activeEmulatorInstance().deactivateInput().catch(() => {});
    return () => {
      void activeEmulatorInstance().activateInput().catch(() => {});
    };
  }, [remappingOpen]);

  useEffect(() => {
    let subscribed = true;
    let observing = false;
    let observed = false;
    let retryTimer: ReturnType<typeof setTimeout> | null = null;
    let instance: BackgroundRemoteValue | null = null;
    let binding = 0;

    const updateAudio = (value: unknown) => {
      if (subscribed) setAudio(audioState(value));
    };

    const ready = () => {
      if (!subscribed) return;
      updatePhase("running");
      setStatus("Game is running");
      setError("");
    };

    const failed = (value: unknown) => {
      if (!subscribed) return;
      const message =
        value && typeof value === "object" && "message" in value
          ? String(value.message)
          : "Game launch failed";
      updatePhase("idle");
      setStatus(message);
      setError(message);
    };

    const closed = () => {
      if (!subscribed) return;
      updatePhase("idle");
      setStatus("Game closed");
      setAudio(DEFAULT_AUDIO);
      setConfirmClose(false);
      setRemappingOpen(false);
    };

    const detach = (target: BackgroundRemoteValue | null) => {
      if (!target) return;
      void target.off("ready", ready).catch(() => {});
      void target.off("audioChanged", updateAudio).catch(() => {});
      void target.off("error", failed).catch(() => {});
      void target.off("closed", closed).catch(() => {});
    };

    const bindActiveInstance = async () => {
      const currentBinding = ++binding;
      const resolved = (await activeEmulatorInstance()) as unknown;
      if (!subscribed || currentBinding !== binding) return;

      detach(instance);
      instance = resolved ? (resolved as BackgroundRemoteValue) : null;
      if (!instance) {
        updatePhase("idle");
        setAudio(DEFAULT_AUDIO);
        setConfirmClose(false);
        setRemappingOpen(false);
        return;
      }

      setError("");
      await Promise.all([
        instance.on("ready", ready),
        instance.on("audioChanged", updateAudio),
        instance.on("error", failed),
        instance.on("closed", closed),
      ]);
      if (!subscribed || currentBinding !== binding) return;

      updateAudio((await instance.audio) as unknown);
      const nativeState = String(await instance.state);
      if (!subscribed || currentBinding !== binding) return;
      if (nativeState === "ready") {
        ready();
      } else if (nativeState === "closing") {
        updatePhase("closing");
        setStatus("Closing game");
      } else if (nativeState === "error") {
        const remoteError = (await instance.error) as unknown;
        const message = remoteError
          ? String(await (remoteError as BackgroundRemoteValue).message)
          : "Game launch failed";
        failed({ message });
      } else if (nativeState === "closed") {
        closed();
      } else {
        updatePhase("launching");
        setStatus("Launching game");
      }
    };

    const activeChanged = () => {
      void bindActiveInstance().catch((bindingError: unknown) => {
        if (subscribed) {
          console.error("Could not bind the active emulator", bindingError);
        }
      });
    };

    const observeActiveInstance = () => {
      if (!subscribed || observing || observed) return;
      observing = true;
      void (async () => {
        await emulatorApi().addEventListener(
          "activeinstancechange",
          activeChanged,
        );
        observed = true;
        if (!subscribed) {
          await emulatorApi().removeEventListener(
            "activeinstancechange",
            activeChanged,
          );
          return;
        }
        await bindActiveInstance();
      })()
        .catch((subscriptionError: unknown) => {
          if (subscribed) {
            console.error(
              "Could not observe the active emulator",
              subscriptionError,
            );
          }
        })
        .finally(() => {
          observing = false;
          if (
            subscribed &&
            !observed &&
            window.gameLayer.ready &&
            retryTimer === null
          ) {
            retryTimer = setTimeout(() => {
              retryTimer = null;
              observeActiveInstance();
            }, 100);
          }
        });
    };

    window.gameLayer.addEventListener("load", observeActiveInstance);
    if (window.gameLayer.ready) observeActiveInstance();

    return () => {
      subscribed = false;
      binding += 1;
      window.gameLayer.removeEventListener("load", observeActiveInstance);
      if (retryTimer !== null) clearTimeout(retryTimer);
      detach(instance);
      if (observed) {
        void emulatorApi()
          .removeEventListener("activeinstancechange", activeChanged)
          .catch(() => {});
      }
    };
  }, []);

  const toggleHook = (hook: GraphicsHook) => {
    setHooks((selected) =>
      selected.includes(hook)
        ? selected.filter((value) => value !== hook)
        : [...selected, hook],
    );
  };

  const rememberLaunch = (configuration: LaunchConfiguration) => {
    const identity = launchIdentity(configuration);
    setLaunchHistory((entries) => [
      {
        ...configuration,
        arguments: [...configuration.arguments],
        hooks: [...configuration.hooks],
        launchedAt: Date.now(),
      },
      ...entries.filter(
        (entry) =>
          launchIdentity({
            command: entry.command,
            arguments: entry.arguments,
            isolated: entry.isolated,
            hooks: entry.hooks,
            inputMapping: entry.inputMapping,
          }) !== identity,
      ),
    ].slice(0, LAUNCH_HISTORY_LIMIT));
  };

  const launchGame = async (configuration: LaunchConfiguration) => {
    setError("");
    if (!configuration.command.trim()) {
      setError("Choose an executable first.");
      return;
    }
    if (!configuration.hooks.length) {
      setError("Select at least one graphics hook.");
      return;
    }

    const options: LaunchRequest = {
      command: configuration.command.trim(),
      arguments: [...configuration.arguments],
      isolated: configuration.isolated,
      hook: [...configuration.hooks],
      surfaces: [
        { index: 0, rotation: 0, webrtc: false },
        { index: 1, rotation: 0, webrtc: false },
      ],
      gamepadMapping: gamepadMapping(configuration.inputMapping),
    };
    updatePhase("launching");
    setStatus("Launching game");
    try {
      const background = backgroundWindow();
      const surfaces = [];
      for (const surface of options.surfaces) {
        const attach = (await background.document.querySelector(
          `tracked-surface[src="${surface.index}"]`,
        )) as unknown;
        if (!attach) {
          throw new Error(
            `No background tracked-surface exists for target ${surface.index}`,
          );
        }
        surfaces.push({ ...surface, attach });
      }
      const instance = await emulatorApi().launch({
        ...options,
        surfaces,
      });
      if (!instance) {
        throw new Error("Another emulator instance is already active");
      }
      rememberLaunch(configuration);
    } catch (launchError) {
      const message =
        launchError instanceof Error ? launchError.message : String(launchError);
      updatePhase("idle");
      setStatus(message);
      setError(message);
    }
  };

  const startGame = (event: FormEvent<HTMLFormElement>) => {
    event.preventDefault();
    let arguments_: string[];
    try {
      arguments_ = splitArguments(argumentsText);
    } catch (launchError) {
      setError(
        launchError instanceof Error
          ? launchError.message
          : "Could not parse the arguments.",
      );
      return;
    }
    void launchGame({
      command: command.trim(),
      arguments: arguments_,
      isolated,
      hooks,
      inputMapping,
    });
  };

  const relaunch = (entry: LaunchHistoryEntry) => {
    void launchGame({
      command: entry.command,
      arguments: entry.arguments,
      isolated: entry.isolated,
      hooks: entry.hooks,
      inputMapping: entry.inputMapping,
    });
  };

  const editLaunch = (entry: LaunchHistoryEntry) => {
    setCommand(entry.command);
    setArgumentsText(formatArguments(entry.arguments));
    setIsolated(entry.isolated);
    setHooks([...entry.hooks]);
    setInputMapping(entry.inputMapping);
    setError("");
    setStatus(`Loaded ${commandName(entry.command)}`);
  };

  const removeLaunch = (entry: LaunchHistoryEntry) => {
    const identity = launchIdentity(entry);
    setLaunchHistory((entries) =>
      entries.filter((candidate) => launchIdentity(candidate) !== identity),
    );
  };

  const gameActive = isGameView(phase);
  const launchBusy = phase !== "idle";
  const hotbarVisible = gameActive;

  const setGameVolume = (volume: number) => {
    volume = Math.max(0, Math.min(1, volume));
    setAudio((state) => ({ ...state, volume }));
    void activeEmulatorInstance()
      .setVolume(volume)
      .catch((volumeError: unknown) => {
        console.error("Could not set game volume", volumeError);
      });
  };

  const toggleGameMuted = () => {
    const muted = !audio.muted;
    setAudio((state) => ({ ...state, muted }));
    void activeEmulatorInstance()
      .setMuted(muted)
      .catch((muteError: unknown) => {
        console.error("Could not change game mute state", muteError);
      });
  };

  const closeGame = () => {
    setConfirmClose(false);
    updatePhase("closing");
    setStatus("Closing game");
    void activeEmulatorInstance().close().catch((closeError: unknown) => {
      updatePhase("running");
      setError(
        closeError instanceof Error ? closeError.message : String(closeError),
      );
    });
  };

  return (
    <>
      <main className="shell" data-game-active={gameActive ? "true" : "false"}>
        <section className="launcher-panel" aria-label="Game launcher">
          <header className="launcher-header">
            <span className="snowglass-mark" aria-hidden="true">
              ◈
            </span>
            <div>
              <p className="eyebrow">Snowglass</p>
              <h1>Game launcher</h1>
              <p className="launcher-intro">
                Resume a recent setup or configure a new launch.
              </p>
            </div>
          </header>

          <div className="launcher-grid">
            <section className="recent-launches" aria-labelledby="recent-title">
              <div className="section-heading">
                <div>
                  <p className="section-kicker">Library</p>
                  <h2 id="recent-title">Recent launches</h2>
                </div>
                {launchHistory.length > 0 && (
                  <button
                    className="text-button"
                    type="button"
                    onClick={() => setLaunchHistory([])}
                  >
                    Clear
                  </button>
                )}
              </div>

              {launchHistory.length > 0 ? (
                <ul className="history-list">
                  {launchHistory.map((entry) => {
                    const identity = launchIdentity(entry);
                    return (
                      <li className="history-entry" key={identity}>
                        <button
                          className="history-launch"
                          type="button"
                          disabled={launchBusy}
                          onClick={() => relaunch(entry)}
                          title={`Launch ${commandName(entry.command)}`}
                        >
                          <span className="history-icon" aria-hidden="true">
                            <svg viewBox="0 0 24 24">
                              <path d="m9 7 8 5-8 5V7z" />
                            </svg>
                          </span>
                          <span className="history-copy">
                            <strong>{commandName(entry.command)}</strong>
                            <small className="history-command">
                              {entry.command}
                              {entry.arguments.length > 0
                                ? ` ${formatArguments(entry.arguments)}`
                                : ""}
                            </small>
                            <span className="history-meta">
                              <span>{hooksLabel(entry.hooks)}</span>
                              <span>{INPUT_MAPPING_LABELS[entry.inputMapping]}</span>
                              {entry.isolated && <span>Isolated</span>}
                              <time dateTime={new Date(entry.launchedAt).toISOString()}>
                                {launchedLabel(entry.launchedAt)}
                              </time>
                            </span>
                          </span>
                        </button>
                        <div className="history-actions">
                          <button
                            type="button"
                            onClick={() => editLaunch(entry)}
                            title="Load this setup into the form"
                          >
                            Edit
                          </button>
                          <button
                            className="history-remove"
                            type="button"
                            onClick={() => removeLaunch(entry)}
                            aria-label={`Remove ${commandName(entry.command)} from history`}
                            title="Remove from history"
                          >
                            ×
                          </button>
                        </div>
                      </li>
                    );
                  })}
                </ul>
              ) : (
                <div className="history-empty">
                  <span className="history-empty-icon" aria-hidden="true">
                    ◷
                  </span>
                  <h3>No recent games yet</h3>
                  <p>Your successful launches will appear here for one-click replay.</p>
                </div>
              )}
            </section>

            <section className="launch-setup" aria-labelledby="setup-title">
              <div className="section-heading">
                <div>
                  <p className="section-kicker">Configuration</p>
                  <h2 id="setup-title">New launch</h2>
                </div>
              </div>

              <form className="launcher-form" onSubmit={startGame}>
                <label className="field">
                  <span>Command</span>
                  <input
                    type="text"
                    value={command}
                    onChange={(event) => setCommand(event.target.value)}
                    placeholder="C:\Games\Game\game.exe"
                    autoComplete="off"
                    spellCheck={false}
                  />
                </label>

                <label className="field">
                  <span>Arguments</span>
                  <input
                    type="text"
                    value={argumentsText}
                    onChange={(event) => setArgumentsText(event.target.value)}
                    placeholder='--fullscreen --profile "Player One"'
                    autoComplete="off"
                    spellCheck={false}
                  />
                </label>

                <fieldset className="hooks">
                  <legend>Graphics hooks</legend>
                  <div className="hook-options">
                    {HOOKS.map((hook) => (
                      <label key={hook.id} className="check-option">
                        <input
                          type="checkbox"
                          checked={hooks.includes(hook.id)}
                          onChange={() => toggleHook(hook.id)}
                        />
                        <span>{hook.label}</span>
                      </label>
                    ))}
                  </div>
                </fieldset>

                <div className="form-row">
                  <label className="field mapping-field">
                    <span>Input mapping</span>
                    <select
                      value={inputMapping}
                      onChange={(event) =>
                        setInputMapping(event.target.value as InputMapping)
                      }
                    >
                      <option value="controller">Controller 1</option>
                      <option value="keyboard">Keyboard</option>
                      <option value="keyboard-mouse">Keyboard + mouse</option>
                      <option value="none">None</option>
                    </select>
                  </label>

                  <label className="isolate-option">
                    <input
                      type="checkbox"
                      checked={isolated}
                      onChange={(event) => setIsolated(event.target.checked)}
                    />
                    <span>
                      <strong>Isolated desktop</strong>
                      <small>Keep the game separate from your desktop</small>
                    </span>
                  </label>
                </div>

                <output
                  className="launcher-message"
                  data-error={error ? "true" : "false"}
                >
                  {error || status}
                </output>

                <button
                  className="start-button"
                  type="submit"
                  disabled={launchBusy}
                >
                  <svg viewBox="0 0 24 24" aria-hidden="true">
                    <path d="m9 7 8 5-8 5V7z" />
                  </svg>
                  Start game
                </button>
              </form>
            </section>
          </div>
        </section>
      </main>

      <div
        className="game-controls-layer"
        data-visible={hotbarVisible ? "true" : "false"}
      >
        <div
          className="game-hotbar-trigger"
          data-snowglass-foreground-hit-region
        >
          <div
            className="game-hotbar"
            role="toolbar"
            aria-label="Game controls"
          >
            <button
              className="hotbar-button hotbar-power"
              type="button"
              aria-label="Close game"
              title="Close game"
              onClick={() => setConfirmClose(true)}
            >
              <svg viewBox="0 0 24 24" aria-hidden="true">
                <path d="M12 2.5v9" />
                <path d="M6.35 5.65a8 8 0 1 0 11.3 0" />
              </svg>
            </button>
            <span className="hotbar-divider" />
            <button
              className="hotbar-button hotbar-remapping"
              type="button"
              aria-label="Controller mapping"
              aria-pressed={remappingOpen}
              title="Controller mapping"
              onClick={() => setRemappingOpen(true)}
            >
              <svg viewBox="0 0 24 24" aria-hidden="true">
                <path d="M7.5 8h9a4.5 4.5 0 0 1 4.2 6.1l-1.1 2.8a2 2 0 0 1-3.2.8L14.7 16H9.3l-1.7 1.7a2 2 0 0 1-3.2-.8l-1.1-2.8A4.5 4.5 0 0 1 7.5 8Z" />
                <path d="M7 11v4M5 13h4" />
                <circle cx="16.5" cy="12" r=".7" />
                <circle cx="18.5" cy="14" r=".7" />
              </svg>
            </button>
            <button
              className="hotbar-button hotbar-audio"
              type="button"
              aria-label={audio.muted ? "Unmute game" : "Mute game"}
              aria-pressed={audio.muted}
              title={audio.muted ? "Unmute" : "Mute"}
              onClick={toggleGameMuted}
            >
              <svg viewBox="0 0 24 24" aria-hidden="true">
                <path d="M4 9v6h4l5 4V5L8 9H4z" className="speaker-body" />
                {audio.muted || audio.volume === 0 ? (
                  <>
                    <path d="m17 9 5 5" />
                    <path d="m22 9-5 5" />
                  </>
                ) : (
                  <>
                    <path d="M16 8.5a5 5 0 0 1 0 7" />
                    {audio.volume > 0.55 && (
                      <path d="M18.5 6a8.5 8.5 0 0 1 0 12" />
                    )}
                  </>
                )}
              </svg>
            </button>
            <input
              className="hotbar-volume"
              type="range"
              min="0"
              max="1"
              step="0.01"
              value={audio.volume}
              aria-label="Game volume"
              title={`Volume ${Math.round(audio.volume * 100)}%`}
              onChange={(event) => setGameVolume(Number(event.target.value))}
            />
          </div>
        </div>
      </div>

      {remappingOpen && (
        <InputRemapping onClose={() => setRemappingOpen(false)} />
      )}

      {confirmClose && (
        <div
          className="close-confirmation"
          role="presentation"
          data-snowglass-foreground-hit-region
        >
          <section
            className="close-confirmation-dialog"
            role="dialog"
            aria-modal="true"
            aria-label="Close game?"
          >
            <h1>Close this game?</h1>
            <p>Any unsaved progress will be lost.</p>
            <div className="close-confirmation-actions">
              <button
                className="dialog-button secondary"
                type="button"
                onClick={() => setConfirmClose(false)}
              >
                Keep playing
              </button>
              <button
                className="dialog-button danger"
                type="button"
                onClick={closeGame}
              >
                Close game
              </button>
            </div>
          </section>
        </div>
      )}
    </>
  );
}
