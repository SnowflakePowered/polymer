import { useEffect, useState, type FormEvent } from "react";
import type {
  GamePadMapping,
  GraphicsHook,
  LaunchRequest,
} from "./gameview";
import "./gameview";
import "./shell.css";

type InputMapping = "none" | "controller" | "keyboard" | "keyboard-mouse";
type ForegroundPhase = "idle" | "launching" | "running" | "closing";

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

  return (
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
  );
}
