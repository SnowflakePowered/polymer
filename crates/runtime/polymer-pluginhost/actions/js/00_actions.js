import {
  op_action_activate,
  op_actions_register,
  op_configuration_actions,
} from "ext:core/ops";

let currentActions = Object.freeze(Object.create(null));
const actions = new Proxy(Object.create(null), {
  get: (_, name) => currentActions[name],
  has: (_, name) => name in currentActions,
  ownKeys: () => Reflect.ownKeys(currentActions),
  getOwnPropertyDescriptor: (_, name) => name in currentActions
    ? { value: currentActions[name], enumerable: true, configurable: true, writable: false }
    : undefined,
  set: () => false,
  defineProperty: () => false,
  deleteProperty: () => false,
  setPrototypeOf: () => false,
  preventExtensions: () => false,
});

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
  currentActions = Object.freeze(actions);
}

Object.defineProperty(globalThis, "__polymerActions", {
  value: Object.freeze({ create, actions }),
  configurable: false,
  enumerable: false,
  writable: false,
});
