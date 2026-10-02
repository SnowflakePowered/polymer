import { createPackage } from "ext:polymer_digest/00_digest.js";

async function launch(entrypoint, args, configurationId, explicitDefault) {
  const module = await import(entrypoint);
  const receiver = !explicitDefault && typeof module.onLaunch === "function"
    ? module
    : module.default;
  const onLaunch = receiver?.onLaunch;
  if (typeof onLaunch !== "function") {
    throw new TypeError("plugin export 'onLaunch' is not a function");
  }

  const launchSet = args.launchSet;
  if (!Array.isArray(launchSet) || launchSet.length === 0) {
    throw new TypeError("onLaunch requires a nonempty launchSet");
  }
  if (launchSet.some(disc => disc?.package == null || typeof disc.package !== "object")) {
    throw new TypeError("each launchSet entry requires a package");
  }
  const initialDiscIndex = args.initialDiscIndex ?? 0;
  if (!Number.isInteger(initialDiscIndex) || initialDiscIndex < 0 || initialDiscIndex >= launchSet.length) {
    throw new RangeError("initialDiscIndex is outside the launch set");
  }
  const discs = Object.freeze(launchSet.map(disc => Object.freeze({
    package: __polymerFilesystem.bindPackage(createPackage(disc.package)),
  })));
  args.launchSet = discs;
  args.initialDiscIndex = initialDiscIndex;

  const configuration = __polymerPluginHost.configuration(configurationId);
  args.configuration = configuration;
  args.filesystem = __polymerFilesystem.filesystem;
  args.storage = __polymerFilesystem.storage;
  args.firmwareStore = __polymerFirmware.store;
  args.keyStore = __polymerKeys.reader;
  const parserReceiver = typeof module.onParseHotkey === "function" ? module : receiver;
  const actions = __polymerActions.create(configurationId, parserReceiver?.onParseHotkey);
  args.saveStates = Object.freeze({
    directory: __polymerFilesystem.createSaveStateDirectory(),
    quickResume: __polymerFilesystem.quickResume,
  });

  const controllerReceiver = typeof module.onControllerChanged === "function" ? module : receiver;
  const onControllerChanged = controllerReceiver?.onControllerChanged;
  const pauseReceiver = typeof module.onPauseResume === "function" ? module : receiver;
  const onPauseResume = pauseReceiver?.onPauseResume;
  const stateSaveReceiver = typeof module.onStateSave === "function" ? module : receiver;
  const onStateSave = stateSaveReceiver?.onStateSave;
  const stateResumeReceiver = typeof module.onStateResume === "function" ? module : receiver;
  const onStateResume = stateResumeReceiver?.onStateResume;
  const quitReceiver = typeof module.onQuit === "function" ? module : receiver;
  const onQuit = quitReceiver?.onQuit;
  const discReceiver = typeof module.onDiscChange === "function" ? module : receiver;
  const onDiscChange = discReceiver?.onDiscChange;
  const discChanges = launchSet.length > 1 && typeof onDiscChange === "function";

  const result = await Reflect.apply(onLaunch, receiver, [args]);
  const quickResumeAccepted = args.saveStates.quickResume !== null
    && result?.quickResume === args.saveStates.quickResume;
  const quickResumeScreenshot = quickResumeAccepted
    && args.saveStates.quickResume.screenshot !== null
    ? await args.saveStates.quickResume.screenshot.bytes()
    : null;
  const controllerChanges = typeof onControllerChanged === "function"
    || __polymerFilesystem.hasConfigurationMounts();
  const pauseResume = typeof onPauseResume === "function";
  const stateSave = typeof onStateSave === "function";
  const stateResume = typeof onStateResume === "function";
  const quit = typeof onQuit === "function";

  if (controllerChanges || pauseResume || stateSave || stateResume || quit || discChanges) {
    Object.defineProperty(globalThis, "__polymerActiveEmulator", {
      value: Object.freeze({
        actions,
        controllerReceiver,
        onControllerChanged,
        pauseReceiver,
        onPauseResume,
        stateSaveReceiver,
        onStateSave,
        stateResumeReceiver,
        onStateResume,
        quitReceiver,
        onQuit,
        discReceiver,
        onDiscChange,
        launchSet: discs,
        disc: { index: initialDiscIndex },
        saveStateDirectory: args.saveStates.directory,
        launch: Object.freeze({
          console: args.console,
          userConfiguration: args.userConfiguration,
        }),
        configuration,
      }),
      configurable: false,
      enumerable: false,
      writable: false,
    });
  }

  return {
    result,
    configuration,
    controllerChanges,
    pauseResume,
    stateSave,
    stateResume,
    quit,
    discChanges,
    initialDiscIndex,
    quickResumeAccepted,
    quickResumeScreenshot,
  };
}

async function controllerChanged(inputTopology, previousTopology) {
  const active = globalThis.__polymerActiveEmulator;
  if (active == null) throw new TypeError("plugin does not export onControllerChanged");
  const context = __polymerControllerChange.context(
    inputTopology,
    previousTopology,
    active.configuration,
    __polymerFilesystem.filesystem,
    active.actions,
  );
  const result = typeof active.onControllerChanged === "function"
    ? await Reflect.apply(active.onControllerChanged, active.controllerReceiver, [context])
    : undefined;
  __polymerFilesystem.refreshConfigurations();
  return result?.controllerTopology ?? inputTopology;
}

async function pauseResume(pauseState) {
  const active = globalThis.__polymerActiveEmulator;
  if (active == null || typeof active.onPauseResume !== "function") return;
  const context = __polymerControllerChange.pauseContext(pauseState, active.actions);
  await Reflect.apply(active.onPauseResume, active.pauseReceiver, [context]);
}

async function stateAction(slot, save) {
  const active = globalThis.__polymerActiveEmulator;
  const callback = save ? active?.onStateSave : active?.onStateResume;
  const receiver = save ? active?.stateSaveReceiver : active?.stateResumeReceiver;
  if (typeof callback !== "function") {
    throw new DOMException(
      "emulator does not support this save-state action",
      "NotSupportedError",
    );
  }
  const context = __polymerControllerChange.stateContext(
    slot,
    __polymerFilesystem.filesystem,
    active.saveStateDirectory,
    active.actions,
  );
  await Reflect.apply(callback, receiver, [context]);
}

async function discChange(index) {
  const active = globalThis.__polymerActiveEmulator;
  if (active == null || active.launchSet.length < 2 || typeof active.onDiscChange !== "function") {
    throw new DOMException("emulator does not support disc changes", "NotSupportedError");
  }
  if (!Number.isInteger(index) || index < 0 || index >= active.launchSet.length) {
    throw new RangeError("disc index is outside the launch set");
  }
  if (index === active.disc.index) return;
  await Reflect.apply(active.onDiscChange, active.discReceiver, [Object.freeze({
    actions: active.actions,
    launchSet: active.launchSet,
    index,
    filesystem: __polymerFilesystem.filesystem,
  })]);
}

async function quit() {
  const active = globalThis.__polymerActiveEmulator;
  if (active == null || typeof active.onQuit !== "function") return null;
  const value = await Reflect.apply(
    active.onQuit,
    active.quitReceiver,
    [__polymerControllerChange.quitContext(active)],
  );
  if (value == null) return null;
  return await __polymerFilesystem.returnedFile(value);
}

function refreshConfigurations() {
  __polymerFilesystem.refreshConfigurations();
}

Object.defineProperty(globalThis, "__polymerEmulator", {
  value: Object.freeze({
    launch,
    controllerChanged,
    pauseResume,
    stateAction,
    discChange,
    quit,
    refreshConfigurations,
  }),
  configurable: false,
  enumerable: false,
  writable: false,
});
