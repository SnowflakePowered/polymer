async function launch(entrypoint, args, configurationId, explicitDefault) {
  const module = await import(entrypoint);
  const receiver = !explicitDefault && typeof module.onLaunch === "function"
    ? module
    : module.default;
  const onLaunch = receiver?.onLaunch;
  if (typeof onLaunch !== "function") {
    throw new TypeError("plugin export 'onLaunch' is not a function");
  }

  const configuration = __snowglassPluginHost.configuration(configurationId);
  args.configuration = configuration;
  args.filesystem = __snowglassFilesystem.filesystem;
  args.resources = __snowglassFilesystem.resources;
  args.storage = __snowglassFilesystem.storage;
  const parserReceiver = typeof module.onParseHotkey === "function" ? module : receiver;
  const actions = __snowglassActions.create(configurationId, parserReceiver?.onParseHotkey);
  args.actions = actions;
  args.saveStates = Object.freeze({
    directory: __snowglassFilesystem.createSaveStateDirectory(),
    quickResume: __snowglassFilesystem.quickResume,
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

  const result = await Reflect.apply(onLaunch, receiver, [args]);
  const quickResumeAccepted = args.saveStates.quickResume !== null
    && result?.quickResume === args.saveStates.quickResume;
  const quickResumeScreenshot = quickResumeAccepted
    && args.saveStates.quickResume.screenshot !== null
    ? await args.saveStates.quickResume.screenshot.bytes()
    : null;
  const controllerChanges = typeof onControllerChanged === "function"
    || __snowglassFilesystem.hasConfigurationMounts();
  const pauseResume = typeof onPauseResume === "function";
  const stateSave = typeof onStateSave === "function";
  const stateResume = typeof onStateResume === "function";
  const quit = typeof onQuit === "function";

  if (controllerChanges || pauseResume || stateSave || stateResume || quit) {
    Object.defineProperty(globalThis, "__snowglassActiveEmulator", {
      value: Object.freeze({
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
        saveStateDirectory: args.saveStates.directory,
        launch: Object.freeze({
          launchPath: args.launchPath,
          console: args.console,
          package: args.package,
          userConfiguration: args.userConfiguration,
        }),
        configuration,
        actions,
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
    quickResumeAccepted,
    quickResumeScreenshot,
  };
}

async function controllerChanged(inputTopology, previousTopology) {
  const active = globalThis.__snowglassActiveEmulator;
  if (active == null) throw new TypeError("plugin does not export onControllerChanged");
  const context = __snowglassControllerChange.context(
    inputTopology,
    previousTopology,
    active.configuration,
    __snowglassFilesystem.filesystem,
    active.actions,
  );
  const result = typeof active.onControllerChanged === "function"
    ? await Reflect.apply(active.onControllerChanged, active.controllerReceiver, [context])
    : undefined;
  __snowglassFilesystem.refreshConfigurations();
  return result?.controllerTopology ?? inputTopology;
}

async function pauseResume(pauseState) {
  const active = globalThis.__snowglassActiveEmulator;
  if (active == null || typeof active.onPauseResume !== "function") return;
  const context = __snowglassControllerChange.pauseContext(pauseState, active.actions);
  await Reflect.apply(active.onPauseResume, active.pauseReceiver, [context]);
}

async function stateAction(slot, save) {
  const active = globalThis.__snowglassActiveEmulator;
  const callback = save ? active?.onStateSave : active?.onStateResume;
  const receiver = save ? active?.stateSaveReceiver : active?.stateResumeReceiver;
  if (typeof callback !== "function") {
    throw new DOMException(
      "emulator does not support this save-state action",
      "NotSupportedError",
    );
  }
  const context = __snowglassControllerChange.stateContext(
    slot,
    active.actions,
    __snowglassFilesystem.filesystem,
    active.saveStateDirectory,
  );
  await Reflect.apply(callback, receiver, [context]);
}

async function quit() {
  const active = globalThis.__snowglassActiveEmulator;
  if (active == null || typeof active.onQuit !== "function") return null;
  const value = await Reflect.apply(
    active.onQuit,
    active.quitReceiver,
    [__snowglassControllerChange.quitContext(active)],
  );
  if (value == null) return null;
  return await __snowglassFilesystem.returnedFile(value);
}

function refreshConfigurations() {
  __snowglassFilesystem.refreshConfigurations();
}

Object.defineProperty(globalThis, "__snowglassEmulator", {
  value: Object.freeze({
    launch,
    controllerChanged,
    pauseResume,
    stateAction,
    quit,
    refreshConfigurations,
  }),
  configurable: false,
  enumerable: false,
  writable: false,
});
