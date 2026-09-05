import assert from "node:assert/strict";
import { readFile } from "node:fs/promises";
import test from "node:test";

const source = (await readFile(new URL("../crates/runtime/polymer-deno/js/02_plugin.js", import.meta.url), "utf8"))
  .replace('import { op_plugin_invoke, op_plugin_list } from "ext:core/ops";',
    'const op_plugin_list = () => []; const op_plugin_invoke = async () => ({ ok: true, value: null });');
const { createPlugin } = await import(`data:text/javascript;base64,${Buffer.from(source).toString("base64")}`);

test("plugin events reach listeners and replay only the latest state for each source and topic", () => {
  const bridge = createPlugin();
  const received = [];
  const listener = event => received.push(event.detail);
  bridge.Plugin.addEventListener("loadprogress", listener);
  const event = { sourceId: 1, pluginId: "launchbox", name: "loadprogress", detail: { state: "loading", completed: 25, total: 100 } };
  bridge.dispatch(event);
  bridge.dispatch({ ...event, sourceId: 2 });
  bridge.dispatch({ ...event, detail: { state: "ready" } });
  bridge.dispatch({ ...event, name: "custom", detail: [1, 2] });
  assert.equal(received.length, 3);
  assert.equal(received[0].detail.completed, 25);
  assert.deepEqual(bridge.Plugin.events().map(value => [value.sourceId, value.name, value.detail]), [
    [2, "loadprogress", event.detail],
    [1, "loadprogress", { state: "ready" }],
    [1, "custom", [1, 2]],
  ]);
  bridge.Plugin.removeEventListener("loadprogress", listener);
  bridge.dispatch(event);
  assert.equal(received.length, 3);
});
