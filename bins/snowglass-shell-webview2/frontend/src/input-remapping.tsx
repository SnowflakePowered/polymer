import * as Ark from "@ark-ui/solid";
import { For, Show, createEffect, createMemo, createSignal, onCleanup } from "solid-js";

const { Tabs } = Ark as any;
type Binding = { kind: "button"; source: { kind: "gamepadButton"; button: string } } | { kind: "axis"; source: { kind: "gamepadAxis"; axis: string }; transform?: { invert?: boolean; scale?: number; deadZone?: number } } | { kind: "disabled" };
type Port = { port: string; layout: string; slot: number | null; source: unknown; overrides: Record<string, Binding> };
type Configuration = { revision: number; machine: string | null; ports: Port[] };
type Layout = { id: string; name: string; elements: Array<{ key: string; label: string; kind: string }> };
type Controller = { assignedPorts: number[]; name: string; buttons: Array<{ button: string; pressed: boolean }>; axes: Array<{ axis: string; value: number }> };
type Control = { key: string; label: string; category: "button" | "axis"; targets: string[] };
type InputApi = EventTarget & { configuration: Configuration; layouts: Layout[]; controllers: Controller[]; configure(value: unknown): Promise<Configuration> };

const friendly = (value: string) => value.replaceAll("_", " ").replace(/\b\w/g, (letter) => letter.toUpperCase());
const snake = (value: string) => value.replace(/([a-z0-9])([A-Z])/g, "$1_$2").replace(/[\s-]+/g, "_").toLowerCase();
const portLabel = (value: string) => { const [group, index] = value.split("."); return `${friendly(group)} ${Number(index) + 1}`; };

function controls(layout?: Layout): Control[] {
  const groups = new Map<string, Control>();
  for (const item of layout?.elements || []) {
    if (!["Button", "Directional", "AxisPositive", "AxisNegative", "Trigger"].includes(item.kind)) continue;
    const category = item.kind.startsWith("Axis") || item.kind === "Trigger" ? "axis" : "button";
    const key = category === "axis" ? item.key.replace(/[+-]$/, "") : item.key;
    const current = groups.get(key);
    if (current) current.targets.push(item.key);
    else groups.set(key, { key, label: item.label || friendly(key.split(".").slice(1).join(" ")), category, targets: [item.key] });
  }
  return [...groups.values()];
}

function bindingLabel(binding?: Binding) {
  if (!binding) return "Default mapping";
  if (binding.kind === "disabled") return "Disabled";
  return friendly(binding.kind === "button" ? binding.source.button : binding.source.axis) + (binding.kind === "axis" && binding.transform?.invert ? " · inverted" : "");
}

export function InputRemapping(props: { onClose(): void }) {
  const api = window.snowglassInput as InputApi;
  const [configuration, setConfiguration] = createSignal<Configuration>(api.configuration || { revision: 0, machine: null, ports: [] });
  const [controllers, setControllers] = createSignal<Controller[]>(api.controllers || []);
  const [selected, setSelected] = createSignal(configuration().ports[0]?.port || "");
  const [capture, setCapture] = createSignal<{ control: Control; buttons: Map<string, boolean>; axes: Map<string, number> } | null>(null);
  const [busy, setBusy] = createSignal<string[]>([]);
  const [message, setMessage] = createSignal("");
  const port = createMemo(() => configuration().ports.find((item) => item.port === selected()) || configuration().ports[0]);
  const layout = createMemo(() => api.layouts?.find((item) => item.id === port()?.layout));
  const groups = createMemo(() => controls(layout()));
  const controller = createMemo(() => controllers().find((item) => port()?.slot != null && item.assignedPorts.includes(port()!.slot!)));

  createEffect(() => {
    const configurationChanged = (event: Event) => setConfiguration((event as CustomEvent<Configuration>).detail || api.configuration);
    const inputChanged = () => {
      setControllers([...(api.controllers || [])]);
      const listening = capture(); const current = controller();
      if (!listening || !current) return;
      if (listening.control.category === "button") {
        const pressed = current.buttons.find((item) => item.pressed && !listening.buttons.get(item.button));
        current.buttons.forEach((item) => listening.buttons.set(item.button, item.pressed));
        if (pressed) void apply(listening.control, { kind: "button", source: { kind: "gamepadButton", button: snake(pressed.button) } });
      } else {
        const moved = current.axes.map((item) => ({ item, delta: Math.abs(item.value - (listening.axes.get(item.axis) ?? item.value)) })).sort((a, b) => b.delta - a.delta)[0];
        if (moved?.delta >= 12000) void apply(listening.control, { kind: "axis", source: { kind: "gamepadAxis", axis: snake(moved.item.axis) }, transform: { invert: false, scale: 1, deadZone: 0 } });
      }
    };
    api.addEventListener("configurationchange", configurationChanged);
    api.addEventListener("inputstatechange", inputChanged);
    onCleanup(() => { api.removeEventListener("configurationchange", configurationChanged); api.removeEventListener("inputstatechange", inputChanged); });
  });

  async function transact(targets: string[], operations: unknown[]) {
    setBusy(targets); setMessage(""); setCapture(null);
    try { setConfiguration(await api.configure({ operations })); }
    catch (error) { setMessage(error instanceof Error ? error.message : String(error)); }
    finally { setBusy([]); }
  }
  async function apply(control: Control, binding: Binding) {
    const current = port(); if (!current) return;
    await transact(control.targets, control.targets.map((target) => ({ type: "setOverride", port: current.port, target, binding })));
  }
  function listen(control: Control) {
    const current = controller();
    if (!current) { setMessage("Connect the controller assigned to this port first."); return; }
    setMessage(""); setCapture({ control, buttons: new Map(current.buttons.map((item) => [item.button, item.pressed])), axes: new Map(current.axes.map((item) => [item.axis, item.value])) });
  }

  return <div class="remap-editor">
    <div class="remap-editor-scroll">
      <Show when={configuration().ports.length} fallback={<p class="rounded-lg bg-snow-800 p-4">No virtual controller ports are active.</p>}>
      <Tabs.Root value={port()?.port} onValueChange={(details: any) => { setSelected(details.value); setCapture(null); }}>
        <Tabs.List class="flex gap-2 border-b border-snow-300 pb-3"><For each={configuration().ports}>{(item) => <Tabs.Trigger value={item.port} class="rounded px-3 py-2 data-[selected]:bg-ice-500 data-[selected]:text-snow-950">{portLabel(item.port)}</Tabs.Trigger>}</For></Tabs.List>
      </Tabs.Root>
      <div class="mt-4 flex justify-between rounded-lg bg-snow-800 p-3"><span>{controller()?.name || "Controller not connected"}</span><span>{layout()?.name || friendly(port()?.layout || "")}</span></div>
      <Show when={capture()}>{(value) => <p role="status" class="mt-3 rounded-lg bg-ice-500 p-3 text-snow-950">Listening for {value().control.label}… <button onClick={() => setCapture(null)}>Cancel</button></p>}</Show>
      <div class="remap-control-list"><For each={groups()}>{(control) => {
        const binding = () => control.targets.map((target) => port()?.overrides[target]).find(Boolean);
        const waiting = () => busy().some((target) => control.targets.includes(target));
        return <article class="remap-control-row"><div class="remap-control-description"><strong>{control.label}</strong><small class="block text-snow-300">{control.targets.join(" + ")}</small></div><button class="remap-binding-button" disabled={waiting()} onClick={() => listen(control)}>{waiting() ? "Applying…" : bindingLabel(binding())}</button><button class="remap-row-action symbol-glyph" disabled={!binding()} title="Restore default" aria-label={`Restore default mapping for ${control.label}`} onClick={() => void transact(control.targets, control.targets.map((target) => ({ type: "resetTarget", port: port()!.port, target })))}>↺</button><button class="remap-row-action symbol-glyph" disabled={binding()?.kind === "disabled"} title="Disable" aria-label={`Disable ${control.label}`} onClick={() => void apply(control, { kind: "disabled" })}>—</button></article>;
      }}</For></div>
      </Show>
      <Show when={message()}><p role="alert" class="mt-3 rounded-lg bg-error-500 p-3">{message()}</p></Show>
    </div>
    <footer class="remap-footer text-sm text-snow-300"><span>Revision {configuration().revision}</span><button class="remap-done" type="button" onClick={() => props.onClose()}>Done</button></footer>
  </div>;
}
