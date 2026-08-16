globalThis.__snowglassCreateConfigurationFacade = (ops, scraping) => {
  const Configuration = Object.freeze({
    describe(packageValue, options = undefined) {
      return ops.op_configuration_describe(
        scraping.serialize(packageValue),
        options?.emulator == null ? null : String(options.emulator),
      );
    },
    prepare(packageValue, selection) {
      return ops.op_configuration_prepare(scraping.serialize(packageValue), selection);
    },
    async setupLaunch(packageValue, selection) {
      const prepared = await Configuration.prepare(packageValue, selection);
      const plugin = await globalThis.Snowglass.plugin.open(prepared.plugin).setupLaunch(prepared.arguments);
      const targets = new Set(prepared.webrtcTargets || []);
      return Object.freeze({
        command: plugin.command,
        arguments: [...(plugin.argv || [])],
        isolated: Boolean(plugin.isolate),
        hook: [...(plugin.hooks || [])],
        surfaces: (plugin.surfaces || []).map((surface) => Object.freeze({
          index: surface.index,
          rotation: surface.rotation || 0,
          selector: surface.pattern,
          webrtc: targets.has(surface.index),
        })),
        gamepadMapping: prepared.gamepadMapping,
        machine: prepared.machine,
        webrtcBind: prepared.webrtcBind,
        generatedConfiguration: {
          root: plugin.configRoot,
          files: plugin.config,
        },
      });
    },
  });
  return Configuration;
};
