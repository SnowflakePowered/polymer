import { For, Show, createEffect, createMemo, createSignal, onCleanup } from "solid-js";
import type { ControllerMappingProfile, InputState, ProjectionMode } from "../../../../js/snowglass";

type Layout = { id: string; name: string; elements: Array<{ key: string; label: string; kind: string }> };

const friendly = (value: string) => value.replaceAll("_", " ").replace(/\b\w/g, (letter) => letter.toUpperCase());
const buttonExpressions = [
  "button.a", "button.b", "button.x", "button.y", "button.select", "button.guide", "button.start",
  "button.stick.left", "button.stick.right", "button.l", "button.r", "directional.n", "directional.s",
  "directional.w", "directional.e", "button.misc1", "button.paddle.right.1", "button.paddle.left.1",
  "button.paddle.right.2", "button.paddle.left.2", "touchpad",
];

const activeControl = (state: InputState) => {
  const button = buttonExpressions.findIndex((_, index) => (state.buttons & (1 << index)) !== 0);
  if (button >= 0) return { expression: buttonExpressions[button], value: 1 };
  const axes = [
    ["analog.stick.left.x+", state.leftStick[0]], ["analog.stick.left.y+", state.leftStick[1]],
    ["analog.stick.right.x+", state.rightStick[0]], ["analog.stick.right.y+", state.rightStick[1]],
    ["trigger.left", state.leftTrigger], ["trigger.right", state.rightTrigger],
  ] as const;
  const axis = axes.find(([, value]) => Math.abs(value) >= 12000);
  return axis && { expression: axis[0], value: axis[1] };
};

export function InputRemapping(props: { onClose(): void }) {
  const api = window.Snowglass.controllers;
  const layouts = api.layouts as readonly Layout[];
  const [revision, setRevision] = createSignal(0);
  const [deviceId, setDeviceId] = createSignal(api.connected[0]?.id || "");
  const [layoutId, setLayoutId] = createSignal(layouts.find((layout) => layout.id === "XBOX_CONTROLLER")?.id || layouts[0]?.id || "");
  const [profile, setProfile] = createSignal<ControllerMappingProfile | null>(null);
  const [capture, setCapture] = createSignal<string | null>(null);
  const [busy, setBusy] = createSignal(false);
  const [message, setMessage] = createSignal("");
  const devices = () => (revision(), api.connected);
  const layout = createMemo(() => layouts.find((candidate) => candidate.id === layoutId()));
  const targets = createMemo(() => layout()?.elements.filter((element) =>
    profile()?.mapping[element.key] !== undefined
  ) || []);

  const load = async () => {
    if (!deviceId() || !layoutId()) {
      setProfile(null);
      return;
    }
    try {
      setProfile(await api.getMapping(deviceId(), layoutId()));
      setMessage("");
    } catch (error) {
      setMessage(error instanceof Error ? error.message : String(error));
    }
  };

  createEffect(() => {
    deviceId();
    layoutId();
    void load();
  });

  createEffect(() => {
    const stateChanged = () => {
      setRevision((value) => value + 1);
      if (!devices().some((device) => device.id === deviceId())) setDeviceId(devices()[0]?.id || "");
    };
    const input = (event: Event) => {
      const target = capture();
      const { index } = (event as CustomEvent<{ index: number }>).detail;
      const device = api.players.find((player) => player.index === index)?.controller;
      const state = window.Snowglass.input.get(index);
      const detail = state && activeControl(state);
      if (!target || device?.id !== deviceId() || !detail) return;
      let source = detail.expression;
      if (source.endsWith("+")) {
        const positive = source.includes(".y+") ? detail.value < 0 : detail.value > 0;
        if (!positive) source = `${source.slice(0, -1)}-`;
      }
      setCapture(null);
      void applyMapping(target, source);
    };
    api.addEventListener("statechange", stateChanged);
    window.Snowglass.input.addEventListener("input", input);
    onCleanup(() => {
      api.removeEventListener("statechange", stateChanged);
      window.Snowglass.input.removeEventListener("input", input);
    });
  });

  const save = async (mode: ProjectionMode, mapping = profile()?.mapping || {}) => {
    if (!deviceId() || !layoutId()) return;
    setBusy(true);
    setMessage("");
    try {
      setProfile(await api.setMapping(deviceId(), layoutId(), { mode, mapping }));
    } catch (error) {
      setMessage(error instanceof Error ? error.message : String(error));
    } finally {
      setBusy(false);
    }
  };
  const applyMapping = (target: string, source: string) => save("custom", {
    ...(profile()?.mapping || {}),
    [target]: source,
  });
  const reset = async () => {
    if (!deviceId() || !layoutId()) return;
    setBusy(true);
    try {
      setProfile(await api.resetMapping(deviceId(), layoutId()));
      setMessage("");
    } catch (error) {
      setMessage(error instanceof Error ? error.message : String(error));
    } finally {
      setBusy(false);
    }
  };

  return <div class="remap-editor">
    <div class="remap-editor-scroll">
      <Show when={devices().length} fallback={<p class="rounded-lg bg-snow-800 p-4">Connect a controller to edit its mapping.</p>}>
        <label class="block mt-4">Controller<select value={deviceId()} onChange={(event) => setDeviceId(event.currentTarget.value)}>
          <For each={devices()}>{(device) => <option value={device.id}>{device.name}</option>}</For>
        </select></label>
        <label class="block mt-4">Logical layout<select value={layoutId()} onChange={(event) => setLayoutId(event.currentTarget.value)}>
          <For each={layouts}>{(item) => <option value={item.id}>{item.name || friendly(item.id)}</option>}</For>
        </select></label>
        <div class="mt-4 flex gap-2">
          <button disabled={busy()} onClick={() => void save("name")}>Match names</button>
          <button disabled={busy()} onClick={() => void save("position")}>Match positions</button>
          <button disabled={busy()} onClick={() => void reset()}>Reset</button>
        </div>
        <Show when={capture()}>{(target) => <p role="status" class="mt-3 rounded-lg bg-ice-500 p-3 text-snow-950">Press or move the physical control for {target()}… <button onClick={() => setCapture(null)}>Cancel</button></p>}</Show>
        <div class="remap-control-list"><For each={targets()}>{(target) => <article class="remap-control-row">
          <div class="remap-control-description"><strong>{target.label}</strong><small class="block text-snow-300">{target.key}</small></div>
          <button class="remap-binding-button" disabled={busy()} onClick={() => setCapture(target.key)}>{profile()?.mapping[target.key] || "none"}</button>
          <button class="remap-row-action symbol-glyph" disabled={busy()} title="Disable" onClick={() => void applyMapping(target.key, "none")}>—</button>
        </article>}</For></div>
      </Show>
      <Show when={message()}><p role="alert" class="mt-3 rounded-lg bg-error-500 p-3">{message()}</p></Show>
    </div>
    <footer class="remap-footer text-sm text-snow-300"><span>{profile()?.mode || "name"} · {profile()?.scope || "device"}</span><button class="remap-done" type="button" onClick={props.onClose}>Done</button></footer>
  </div>;
}
