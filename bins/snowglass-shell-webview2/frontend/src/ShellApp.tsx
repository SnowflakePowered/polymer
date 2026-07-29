import { useEffect, useState, type FormEvent } from "react";
import type {
  BackgroundRemoteValue,
  GamePadMapping,
  GraphicsHook,
  LaunchRequest,
} from "./gameview";
import "./gameview";
import "./shell.css";

type InputMapping = "none" | "controller" | "keyboard" | "keyboard-mouse";
type ForegroundPhase = "idle" | "launching" | "running" | "closing";
type GameAudioState = {
  available: boolean;
  volume: number;
  muted: boolean;
};

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

function activeGameInstance(): BackgroundRemoteValue {
  return window.gameLayer.contentWindow.gameView.activeInstance;
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
  const [status, setStatus] = useState("Choose a game to start");
  const [error, setError] = useState("");
  const [audio, setAudio] = useState<GameAudioState>(DEFAULT_AUDIO);
  const [confirmClose, setConfirmClose] = useState(false);

  useEffect(() => {
    const listener = (event: Event) => {
      const message = (event as MessageEvent).data;
      if (
        message?.type === "gameViewStatus" &&
        typeof message.status === "string"
      ) {
        setStatus(message.status);
      }
      if (message?.type !== "gameViewLifecycle") return;
      if (typeof message.status === "string") setStatus(message.status);
      if (
        message.phase === "launching" ||
        message.phase === "running" ||
        message.phase === "closing"
      ) {
        setPhase(message.phase);
        setError("");
      } else if (message.phase === "closed" || message.phase === "error") {
        setPhase("idle");
        setAudio(DEFAULT_AUDIO);
        setConfirmClose(false);
        setError(
          message.phase === "error" && typeof message.error === "string"
            ? message.error
            : "",
        );
      }
    };
    window.gameLayer.addEventListener("message", listener);
    return () => window.gameLayer.removeEventListener("message", listener);
  }, []);

  useEffect(() => {
    if (phase !== "running") return;
    let subscribed = true;
    let instance: BackgroundRemoteValue | null = null;
    const updateAudio = (value: unknown) => {
      if (subscribed) setAudio(audioState(value));
    };
    void (async () => {
      const resolved = (await activeGameInstance()) as unknown;
      if (!subscribed || !resolved) return;
      instance = resolved as BackgroundRemoteValue;
      await instance.on("audioChanged", updateAudio);
      if (subscribed) {
        const initial = (await instance.audio) as unknown;
        updateAudio(initial);
      }
    })().catch((subscriptionError: unknown) => {
      if (subscribed) {
        console.error("Could not subscribe to game audio", subscriptionError);
      }
    });
    return () => {
      subscribed = false;
      if (instance) {
        void instance.off("audioChanged", updateAudio).catch(() => {});
      }
    };
  }, [phase]);

  const toggleHook = (hook: GraphicsHook) => {
    setHooks((selected) =>
      selected.includes(hook)
        ? selected.filter((value) => value !== hook)
        : [...selected, hook],
    );
  };

  const startGame = (event: FormEvent<HTMLFormElement>) => {
    event.preventDefault();
    setError("");
    if (!command.trim()) {
      setError("Choose an executable first.");
      return;
    }
    if (!hooks.length) {
      setError("Select at least one graphics hook.");
      return;
    }

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

    const options: LaunchRequest = {
      command: command.trim(),
      arguments: arguments_,
      isolated,
      hook: hooks,
      surfaces: [
        { index: 0, rotation: 0, webrtc: false },
        { index: 1, rotation: 0, webrtc: false },
      ],
      gamepadMapping: gamepadMapping(inputMapping),
    };
    setPhase("launching");
    setStatus("Launching game");
    window.gameLayer.postMessage({
      type: "snowglass-browser-launch",
      options,
    });
  };

  const gameActive = phase !== "idle";
  const hotbarVisible = phase === "running";

  const setGameVolume = (volume: number) => {
    volume = Math.max(0, Math.min(1, volume));
    setAudio((state) => ({ ...state, volume }));
    void activeGameInstance().setVolume(volume).catch((volumeError: unknown) => {
      console.error("Could not set game volume", volumeError);
    });
  };

  const toggleGameMuted = () => {
    const muted = !audio.muted;
    setAudio((state) => ({ ...state, muted }));
    void activeGameInstance().setMuted(muted).catch((muteError: unknown) => {
      console.error("Could not change game mute state", muteError);
    });
  };

  const closeGame = () => {
    setConfirmClose(false);
    setPhase("closing");
    setStatus("Closing game");
    void activeGameInstance().close().catch((closeError: unknown) => {
      setPhase("running");
      setError(
        closeError instanceof Error ? closeError.message : String(closeError),
      );
    });
  };

  return (
    <>
      <main className="shell" data-game-active={gameActive ? "true" : "false"}>
        <section className="launcher-panel" aria-label="Start a game">
        <header>
          <span className="snowglass-mark" aria-hidden="true">
            ◈
          </span>
          <div>
            <p className="eyebrow">Snowglass</p>
            <h1>Start a game</h1>
          </div>
        </header>

        <form onSubmit={startGame}>
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

          <button className="start-button" type="submit">
            Start game
          </button>
        </form>
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
