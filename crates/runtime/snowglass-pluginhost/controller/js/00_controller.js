import { uiAutomation } from "ext:snowglass_automation/00_automation.js";

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

function stateContext(slot, actions, filesystem, directory) {
  return Object.freeze({ slot, actions, ui: uiAutomation, filesystem, directory });
}

function quitContext(active) {
  return Object.freeze({
    actions: active.actions,
    ui: uiAutomation,
    filesystem: __snowglassFilesystem.filesystem,
    directory: active.saveStateDirectory,
    launchPath: active.launch.launchPath,
    console: active.launch.console,
    package: active.launch.package,
    userConfiguration: active.launch.userConfiguration,
  });
}

Object.defineProperty(globalThis, "__snowglassControllerChange", {
  value: Object.freeze({ context, pauseContext, stateContext, quitContext }),
  configurable: false,
  enumerable: false,
  writable: false,
});
