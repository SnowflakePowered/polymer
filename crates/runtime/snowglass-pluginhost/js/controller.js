import { uiAutomation } from "./automation.js";

function context(topology, previousTopology, configuration, filesystem, actions) {
  return Object.freeze({
    topology: Object.freeze(topology),
    previousTopology: Object.freeze(previousTopology),
    configuration,
    filesystem,
    ui: uiAutomation,
    actions,
  });
}

function pauseContext(pauseState, actions) {
  return Object.freeze({ pauseState, actions, ui: uiAutomation });
}

Object.defineProperty(globalThis, "__snowglassControllerChange", {
  value: Object.freeze({ context, pauseContext }),
  configurable: false,
  enumerable: false,
  writable: false,
});
