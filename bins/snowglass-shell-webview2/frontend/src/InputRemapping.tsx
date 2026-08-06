import { useEffect, useMemo, useRef, useState } from "react";

type SourceSelector =
  | { kind: "keyboardMouse" }
  | { kind: "sdlIndex"; index: number }
  | { kind: "sdlName"; name: string }
  | { kind: "sdlGuid"; guid: string; duplicateOrdinal: number }
  | { kind: "device"; deviceId: string };

type Binding =
  | {
      kind: "button";
      source: { kind: "gamepadButton"; button: string };
    }
  | {
      kind: "axis";
      source: { kind: "gamepadAxis"; axis: string };
      transform?: { invert?: boolean; scale?: number; deadZone?: number };
    }
  | { kind: "disabled" };

type InputPort = {
  port: number;
  layout: string;
  source: SourceSelector | null;
  overrides: Record<string, Binding>;
};

type InputConfiguration = {
  revision: number;
  ports: InputPort[];
};

type InputElement = {
  id: string;
  label: string;
  kind: string;
};

// A consolespec inputspec, with its clusters already expanded into elements by
// the host. `name` is the product's name from the document's [meta], falling
// back to the id for handhelds, whose built-in controls are not a product.
type InputLayout = {
  id: string;
  name: string;
  elements: InputElement[];
};

type PhysicalController = {
  index: number;
  deviceId: string;
  name: string;
  guid: string;
  assignedPorts: number[];
  buttons: Array<{ button: string; pressed: boolean; mapped: boolean }>;
  axes: Array<{ axis: string; value: number; mapped: boolean }>;
};

type InputApi = EventTarget & {
  readonly configuration: InputConfiguration;
  readonly layouts: InputLayout[];
  readonly controllers: PhysicalController[];
  configure(transaction: {
    expectedRevision?: number;
    operations: unknown[];
  }): Promise<InputConfiguration>;
};

type ControlGroup = {
  key: string;
  label: string;
  category: "button" | "axis";
  targets: string[];
};

type Capture = {
  control: ControlGroup;
  baselineButtons: Map<string, boolean>;
  baselineAxes: Map<string, number>;
};

function inputApi() {
  return (
    window as typeof window & {
      readonly snowglassInput: InputApi;
    }
  ).snowglassInput;
}

function snakeCase(value: string) {
  return value
    .replace(/([a-z0-9])([A-Z])/g, "$1_$2")
    .replace(/[\s-]+/g, "_")
    .toLowerCase();
}

function friendlyControl(value: string) {
  return value
    .replace(/([a-z0-9])([A-Z])/g, "$1 $2")
    .replaceAll("_", " ");
}

function sourceLabel(source: SourceSelector | null) {
  if (!source) return "No source";
  switch (source.kind) {
    case "keyboardMouse":
      return "Keyboard + mouse";
    case "sdlIndex":
      return `SDL controller ${source.index + 1}`;
    case "sdlName":
      return source.name;
    case "sdlGuid":
      return `${source.guid.slice(0, 8)} · copy ${source.duplicateOrdinal + 1}`;
    case "device":
      return source.deviceId;
  }
}

function controlGroups(layout?: InputLayout) {
  if (!layout) return [];
  const groups = new Map<string, ControlGroup>();
  for (const element of layout.elements) {
    const supported = ["Button", "Directional", "AxisPositive", "AxisNegative", "Trigger"];
    if (!supported.includes(element.kind)) continue;
    const isAxis =
      element.kind.startsWith("Axis") || element.kind === "Trigger";
    const axisKey = element.id.replace(/(Positive|Negative)/, "");
    const key = isAxis ? axisKey : element.id;
    const existing = groups.get(key);
    if (existing) {
      existing.targets.push(element.id);
      continue;
    }
    groups.set(key, {
      key,
      label: isAxis
        ? friendlyControl(axisKey.replace(/^Axis/, ""))
        : element.label,
      category: isAxis ? "axis" : "button",
      targets: [element.id],
    });
  }
  return [...groups.values()].sort((left, right) => {
    if (left.category !== right.category) {
      return left.category === "button" ? -1 : 1;
    }
    return left.label.localeCompare(right.label);
  });
}

function bindingFor(port: InputPort, control: ControlGroup) {
  return control.targets
    .map((target) => port.overrides[target])
    .find((binding): binding is Binding => Boolean(binding));
}

function bindingLabel(binding: Binding | undefined) {
  if (!binding) return "Default mapping";
  if (binding.kind === "disabled") return "Disabled";
  if (binding.kind === "button") {
    return friendlyControl(binding.source.button);
  }
  const inverted = binding.transform?.invert ? " · inverted" : "";
  return `${friendlyControl(binding.source.axis)}${inverted}`;
}

function controllerForPort(
  controllers: PhysicalController[],
  port: number,
) {
  return controllers.find((controller) =>
    controller.assignedPorts.includes(port),
  );
}

function currentConfiguration(api: InputApi): InputConfiguration {
  const configuration = api.configuration;
  return {
    revision: Number(configuration?.revision) || 0,
    ports: Array.isArray(configuration?.ports) ? configuration.ports : [],
  };
}

export function InputRemapping({ onClose }: { onClose(): void }) {
  const api = inputApi();
  const [configuration, setConfiguration] = useState(() =>
    currentConfiguration(api),
  );
  const [controllers, setControllers] = useState<PhysicalController[]>(() =>
    Array.isArray(api.controllers) ? [...api.controllers] : [],
  );
  const [selectedPort, setSelectedPort] = useState(
    () => currentConfiguration(api).ports[0]?.port ?? 0,
  );
  const [capture, setCapture] = useState<Capture | null>(null);
  const [busyTargets, setBusyTargets] = useState<string[]>([]);
  const [message, setMessage] = useState("");
  const capturePending = useRef(false);

  const port =
    configuration.ports.find((candidate) => candidate.port === selectedPort) ??
    configuration.ports[0];
  const layout = api.layouts.find((candidate) => candidate.id === port?.layout);
  const groups = useMemo(() => controlGroups(layout), [layout]);
  const controller = port
    ? controllerForPort(controllers, port.port)
    : undefined;

  useEffect(() => {
    const configurationChanged = (event: Event) => {
      const detail = (event as CustomEvent<InputConfiguration>).detail;
      setConfiguration(detail || currentConfiguration(api));
    };
    const inputChanged = () => {
      setControllers(
        Array.isArray(api.controllers) ? [...api.controllers] : [],
      );
    };
    api.addEventListener("configurationchange", configurationChanged);
    api.addEventListener("inputstatechange", inputChanged);
    return () => {
      api.removeEventListener("configurationchange", configurationChanged);
      api.removeEventListener("inputstatechange", inputChanged);
    };
  }, [api]);

  useEffect(() => {
    if (
      configuration.ports.length &&
      !configuration.ports.some(({ port }) => port === selectedPort)
    ) {
      setSelectedPort(configuration.ports[0].port);
    }
  }, [configuration, selectedPort]);

  useEffect(() => {
    if (!capture || !port) return;
    const captureInput = () => {
      if (capturePending.current) return;
      const active = controllerForPort(api.controllers, port.port);
      if (!active) return;
      if (capture.control.category === "button") {
        const pressed = active.buttons.find((button) => {
          const newlyPressed =
            button.pressed &&
            !capture.baselineButtons.get(button.button);
          capture.baselineButtons.set(button.button, button.pressed);
          return newlyPressed;
        });
        if (pressed) {
          capturePending.current = true;
          void applyBinding(capture.control, {
            kind: "button",
            source: {
              kind: "gamepadButton",
              button: snakeCase(pressed.button),
            },
          });
        }
        return;
      }
      const moved = active.axes
        .map((axis) => ({
          axis,
          delta: Math.abs(
            axis.value - (capture.baselineAxes.get(axis.axis) ?? axis.value),
          ),
        }))
        .sort((left, right) => right.delta - left.delta)[0];
      if (moved && moved.delta >= 12_000) {
        capturePending.current = true;
        void applyBinding(capture.control, {
          kind: "axis",
          source: {
            kind: "gamepadAxis",
            axis: snakeCase(moved.axis.axis),
          },
          transform: { invert: false, scale: 1, deadZone: 0 },
        });
      }
    };
    api.addEventListener("inputstatechange", captureInput);
    return () => api.removeEventListener("inputstatechange", captureInput);
  });

  useEffect(() => {
    const keyDown = (event: KeyboardEvent) => {
      if (event.key === "Escape") {
        if (capture) {
          setCapture(null);
          capturePending.current = false;
        } else {
          onClose();
        }
      }
    };
    window.addEventListener("keydown", keyDown);
    return () => window.removeEventListener("keydown", keyDown);
  }, [capture, onClose]);

  const transact = async (targets: string[], operations: unknown[]) => {
    setBusyTargets(targets);
    setMessage("");
    try {
      const updated = await api.configure({ operations });
      setConfiguration(updated);
    } catch (error) {
      setMessage(error instanceof Error ? error.message : String(error));
    } finally {
      setBusyTargets([]);
      setCapture(null);
      capturePending.current = false;
    }
  };

  const applyBinding = async (control: ControlGroup, binding: Binding) => {
    if (!port) return;
    await transact(
      control.targets,
      control.targets.map((target) => ({
        type: "setOverride",
        port: port.port,
        target,
        binding,
      })),
    );
  };

  const reset = (control: ControlGroup) => {
    if (!port) return;
    void transact(
      control.targets,
      control.targets.map((target) => ({
        type: "resetTarget",
        port: port.port,
        target,
      })),
    );
  };

  const disable = (control: ControlGroup) => {
    void applyBinding(control, { kind: "disabled" });
  };

  const toggleInvert = (control: ControlGroup, binding: Binding) => {
    if (binding.kind !== "axis") return;
    void applyBinding(control, {
      ...binding,
      transform: {
        scale: binding.transform?.scale ?? 1,
        deadZone: binding.transform?.deadZone ?? 0,
        invert: !binding.transform?.invert,
      },
    });
  };

  const beginCapture = (control: ControlGroup) => {
    if (!controller) {
      setMessage("Connect the controller assigned to this port first.");
      return;
    }
    capturePending.current = false;
    setMessage("");
    setCapture({
      control,
      baselineButtons: new Map(
        controller.buttons.map(({ button, pressed }) => [button, pressed]),
      ),
      baselineAxes: new Map(
        controller.axes.map(({ axis, value }) => [axis, value]),
      ),
    });
  };

  const pressedCount =
    controller?.buttons.filter(({ pressed }) => pressed).length ?? 0;

  return (
    <div
      className="remapping-overlay"
      role="presentation"
      data-snowglass-foreground-hit-region
    >
      <section
        className="remapping-screen"
        role="dialog"
        aria-modal="true"
        aria-labelledby="remapping-title"
      >
        <header className="remapping-header">
          <div>
            <p className="section-kicker">Live input</p>
            <h1 id="remapping-title">Controller mapping</h1>
            <p>
              Choose a control, then press or move its replacement on the
              assigned controller.
            </p>
          </div>
          <button
            className="remapping-close"
            type="button"
            onClick={onClose}
            aria-label="Close controller mapping"
          >
            ×
          </button>
        </header>

        {configuration.ports.length ? (
          <>
            <nav className="port-tabs" aria-label="Virtual controller ports">
              {configuration.ports.map((candidate) => (
                <button
                  key={candidate.port}
                  type="button"
                  aria-pressed={candidate.port === port?.port}
                  onClick={() => {
                    setSelectedPort(candidate.port);
                    setCapture(null);
                    capturePending.current = false;
                  }}
                >
                  <span>Port {candidate.port + 1}</span>
                  <small>{candidate.layout.replaceAll("_", " ")}</small>
                </button>
              ))}
            </nav>

            <div className="remapping-device">
              <span
                className="device-status"
                data-connected={controller ? "true" : "false"}
              />
              <div>
                <strong>{controller?.name || "Controller not connected"}</strong>
                <small>
                  {sourceLabel(port?.source ?? null)}
                  {controller
                    ? ` · SDL ${controller.index + 1}${
                        pressedCount ? ` · ${pressedCount} pressed` : ""
                      }`
                    : ""}
                </small>
              </div>
              <span className="layout-badge">
                {layout?.name || port?.layout?.replaceAll("_", " ")}
              </span>
            </div>

            {capture && (
              <div className="capture-banner" role="status">
                <span className="capture-pulse" />
                <div>
                  <strong>Listening for {capture.control.label}</strong>
                  <small>
                    {capture.control.category === "button"
                      ? "Press a button"
                      : "Move an axis"}{" "}
                    · Esc to cancel
                  </small>
                </div>
                <button
                  type="button"
                  onClick={() => {
                    setCapture(null);
                    capturePending.current = false;
                  }}
                >
                  Cancel
                </button>
              </div>
            )}

            <div className="mapping-list">
              {groups.map((control) => {
                const binding = bindingFor(port, control);
                const busy = control.targets.some((target) =>
                  busyTargets.includes(target),
                );
                const listening = capture?.control.key === control.key;
                return (
                  <article
                    className="mapping-row"
                    data-listening={listening ? "true" : "false"}
                    key={control.key}
                  >
                    <span
                      className={`mapping-kind mapping-kind-${control.category}`}
                      aria-hidden="true"
                    >
                      {control.category === "button" ? "●" : "↔"}
                    </span>
                    <div className="mapping-target">
                      <strong>{control.label}</strong>
                      <small>{control.targets.join(" + ")}</small>
                    </div>
                    <button
                      className="mapping-binding"
                      type="button"
                      disabled={busy}
                      onClick={() => beginCapture(control)}
                    >
                      <span>{busy ? "Applying…" : bindingLabel(binding)}</span>
                      <small>{listening ? "Listening" : "Change"}</small>
                    </button>
                    {binding?.kind === "axis" && (
                      <button
                        className="mapping-action"
                        type="button"
                        onClick={() => toggleInvert(control, binding)}
                        title="Invert axis"
                        aria-label={`Invert ${control.label}`}
                      >
                        ⇄
                      </button>
                    )}
                    <button
                      className="mapping-action"
                      type="button"
                      onClick={() => reset(control)}
                      disabled={!binding || busy}
                      title="Restore default"
                      aria-label={`Restore default mapping for ${control.label}`}
                    >
                      ↺
                    </button>
                    <button
                      className="mapping-action mapping-disable"
                      type="button"
                      onClick={() => disable(control)}
                      disabled={binding?.kind === "disabled" || busy}
                      title="Disable control"
                      aria-label={`Disable ${control.label}`}
                    >
                      —
                    </button>
                  </article>
                );
              })}
            </div>
          </>
        ) : (
          <div className="remapping-empty">
            <span aria-hidden="true">⌁</span>
            <h2>No virtual controller ports</h2>
            <p>Launch the game with a controller mapping to remap it live.</p>
          </div>
        )}

        <footer className="remapping-footer">
          <span>Revision {configuration.revision}</span>
          <output data-error={message ? "true" : "false"}>
            {message || "Changes apply immediately"}
          </output>
        </footer>
      </section>
    </div>
  );
}
