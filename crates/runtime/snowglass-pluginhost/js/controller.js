import { uiAutomation } from "./automation.js";

function context(topology, previousTopology, configuration, filesystem) {
  return Object.freeze({
    topology: Object.freeze(topology),
    previousTopology: Object.freeze(previousTopology),
    configuration,
    filesystem,
    ui: uiAutomation,
  });
}

Object.defineProperty(globalThis, "__snowglassControllerChange", {
  value: Object.freeze({ context }),
  configurable: false,
  enumerable: false,
  writable: false,
});
