function context(topology, previousTopology, configuration, filesystem, actions) {
  return Object.freeze({
    topology: Object.freeze(topology),
    previousTopology: Object.freeze(previousTopology),
    configuration,
    filesystem,
    actions,
  });
}

function pauseContext(pauseState, actions) {
  return Object.freeze({ pauseState, actions });
}

function stateContext(slot, filesystem, directory, actions) {
  return Object.freeze({ slot, filesystem, directory, actions });
}

function quitContext(active) {
  return Object.freeze({
    actions: active.actions,
    filesystem: __polymerFilesystem.filesystem,
    directory: active.saveStateDirectory,
    console: active.launch.console,
    package: active.launchSet[active.disc.index].package,
    userConfiguration: active.launch.userConfiguration,
  });
}

Object.defineProperty(globalThis, "__polymerControllerChange", {
  value: Object.freeze({ context, pauseContext, stateContext, quitContext }),
  configurable: false,
  enumerable: false,
  writable: false,
});
