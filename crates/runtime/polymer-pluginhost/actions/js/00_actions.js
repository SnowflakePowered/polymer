import {
  op_action_activate,
  op_actions_register,
  op_configuration_actions,
} from "ext:core/ops";

function create(configurationId, parse) {
  const definitions = op_configuration_actions(configurationId);
  const parsed = Object.create(null);
  for (const [name, expression] of Object.entries(definitions)) {
    if (typeof parse !== "function") {
      throw new TypeError(`action '${name}' requires the plugin to export onParseHotkey`);
    }
    const hotkey = Reflect.apply(parse, undefined, [expression]);
    if (hotkey == null || typeof hotkey !== "object") {
      throw new TypeError(`onParseHotkey returned no hotkey for action '${name}'`);
    }
    parsed[name] = hotkey;
  }
  op_actions_register(parsed);

  const actions = Object.create(null);
  for (const name of Object.keys(definitions)) {
    actions[name] = Object.freeze({
      activate() { op_action_activate(name); },
    });
  }
  return Object.freeze(actions);
}

Object.defineProperty(globalThis, "__polymerActions", {
  value: Object.freeze({ create }),
  configurable: false,
  enumerable: false,
  writable: false,
});
