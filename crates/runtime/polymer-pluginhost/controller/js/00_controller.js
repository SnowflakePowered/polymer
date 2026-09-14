function context(topology, previousTopology, configuration, filesystem) {
  return Object.freeze({
    topology: Object.freeze(topology),
    previousTopology: Object.freeze(previousTopology),
    configuration,
    filesystem,
  });
}

function pauseContext(pauseState) {
  return Object.freeze({ pauseState });
}

function stateContext(slot, filesystem, directory) {
  return Object.freeze({ slot, filesystem, directory });
}

function quitContext(active) {
  return Object.freeze({
    filesystem: __polymerFilesystem.forLaunch(active.launchSet[active.disc.index]),
    directory: active.saveStateDirectory,
    launchPath: active.launchSet[active.disc.index].launchPath,
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
