import {
  op_configuration_describe,
  op_configuration_prepare,
} from "ext:core/ops";

export function createConfiguration(scraping, pluginApi) {
  const Configuration = Object.freeze({
    describe(packageValue, options = undefined) {
      return op_configuration_describe(
        scraping.serialize(packageValue),
        options?.emulator == null ? null : String(options.emulator),
      );
    },
    prepare(packageValue, selection) {
      return op_configuration_prepare(scraping.serialize(packageValue), selection);
    },
    async onLaunch(packageValue, selection) {
      const prepared = await Configuration.prepare(packageValue, selection);
      const plugin = await pluginApi.open(prepared.plugin).onLaunch(prepared.arguments);
      const targets = new Set(prepared.webrtcTargets || []);
      const launch = Object.freeze({
        command: plugin.command,
        arguments: [...(plugin.argv || [])],
        isolated: Boolean(plugin.isolate),
        hooks: {
          graphics: [...plugin.hooks.graphics],
          input: [...plugin.hooks.input],
        },
        graphTracking: Boolean(plugin.graphTracking),
        surfaces: (plugin.surfaces || []).map((surface) => Object.freeze({
          index: surface.index,
          rotation: surface.rotation || 0,
          selector: surface.pattern,
          webrtc: targets.has(surface.index),
        })),
        controllerTopology: plugin.controllerTopology ?? prepared.controllerTopology,
        machine: prepared.machine,
        webrtcBind: prepared.webrtcBind,
        preparedFilesystem: plugin.preparedFilesystem,
        controllerSession: plugin.controllerSession,
      });
      return launch;
    },
  });
  return Configuration;
}
