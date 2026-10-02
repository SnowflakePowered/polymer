import { Package } from "./00_package.js";
import {
  op_configuration_configure,
  op_configuration_describe,
  op_configuration_platforms,
  op_configuration_prepare,
  op_configuration_select,
  op_configuration_update_platform,
} from "ext:core/ops";

export function createConfiguration(pluginApi) {
  const sessions = new Map();

  class ConfigurationState {
    #emulator;
    #launchableId;
    #layout;
    #schema;
    #pending = Promise.resolve();
    #revision = 0;
    #values = {};

    constructor(launchableId, emulator) {
      this.#launchableId = launchableId;
      this.#emulator = emulator.id;
      this.#schema = emulator.configuration?.schema;
      this.#accept(emulator.configuration);
    }

    #accept(configured) {
      if (configured === null) {
        this.#layout = null;
        this.#values = {};
        return;
      }
      if (configured.schema !== this.#schema) throw new Error("Plugin configuration schema changed; describe the launchable again");
      const { values, ...layout } = configured;
      this.#layout = layout;
      this.#values = structuredClone(values);
    }

    configure(values = this.#values) {
      const revision = ++this.#revision;
      const input = structuredClone(values);
      const pending = this.#pending.catch(() => {}).then(async () => {
        this.#accept(await op_configuration_configure(this.#launchableId, this.#emulator, input));
        return revision === this.#revision ? this.snapshot() : null;
      });
      this.#pending = pending;
      return pending;
    }

    async settled() {
      for (;;) {
        const pending = this.#pending;
        await pending;
        if (pending === this.#pending) return this.snapshot();
      }
    }

    snapshot() {
      return {
        configuration: this.#layout,
        userConfiguration: structuredClone(this.#values),
      };
    }
  }

  function stateFor(launchableId, emulator, operation) {
    const state = sessions.get(String(launchableId))?.get(String(emulator));
    if (state === undefined) {
      throw new DOMException(
        `Call configuration.describe() before ${operation}()`,
        "InvalidStateError",
      );
    }
    return state;
  }

  const Configuration = Object.freeze({
    async describe(launchableId, options = undefined) {
      for (const state of sessions.get(String(launchableId))?.values() ?? []) {
        await state.settled();
      }
      const description = await op_configuration_describe(
        String(launchableId),
        options?.emulator == null ? null : String(options.emulator),
      );
      const configured = description.candidates.map((emulator) => {
        const state = new ConfigurationState(String(launchableId), emulator);
        Object.assign(emulator, state.snapshot());
        return [emulator.id, state];
      });
      sessions.set(String(launchableId), new Map(configured));
      return description;
    },
    configure(launchableId, emulator, userConfiguration = undefined) {
      return stateFor(launchableId, emulator, "configure").configure(userConfiguration);
    },
    select(launchableId, emulator) {
      return op_configuration_select(String(launchableId), emulator == null ? null : String(emulator));
    },
    platforms: Object.freeze({
      list: () => op_configuration_platforms(),
      update: (platform, settings) => op_configuration_update_platform(String(platform), {
        emulator: settings?.emulator == null ? null : String(settings.emulator),
      }),
    }),
    async prepare(launchableId, selection) {
      await stateFor(launchableId, selection.emulator, "prepare").settled();
      const prepared = await op_configuration_prepare(String(launchableId), selection);
      prepared.launchSet = prepared.launchSet.map(value => new Package(value));
      prepared.arguments.launchSet = prepared.arguments.launchSet.map(disc => Object.freeze({
        ...disc, package: new Package(disc.package),
      }));
      return prepared;
    },
    async onLaunch(launchableId, selection) {
      const launchSet = Array.isArray(launchableId) ? [...launchableId] : [launchableId];
      if (launchSet.length === 0 || launchSet.some(id => typeof id !== "string" || !id)) {
        throw new TypeError("launch requires a nonempty list of launchable IDs");
      }
      const prepared = await Configuration.prepare(launchSet[0], { ...selection, launchSet });
      const plugin = await pluginApi.open(prepared.plugin).onLaunch(prepared.arguments);
      const targets = new Set(prepared.webrtcTargets || []);
      const launch = Object.freeze({
        command: plugin.command,
        workingDirectory: plugin.workingDirectory,
        env: (plugin.env || []).map(entry => [...entry]),
        arguments: [...(plugin.argv || [])],
        isolated: Boolean(plugin.isolate),
        hooks: {
          graphics: [...plugin.hooks.graphics],
          input: [...plugin.hooks.input],
        },
        surfaces: (plugin.surfaces || []).map((surface) => Object.freeze({
          index: surface.index,
          rotation: surface.rotation || 0,
          resolution: surface.resolution,
          selector: surface.pattern,
          webrtc: targets.has(surface.index),
        })),
        controllerTopology: plugin.controllerTopology ?? prepared.controllerTopology,
        machine: prepared.machine,
        webrtcBind: prepared.webrtcBind,
        preparedFilesystem: plugin.preparedFilesystem,
        controllerSession: plugin.controllerSession,
        launchSet: Object.freeze([...prepared.launchSet]),
        initialDiscIndex: plugin.initialDiscIndex,
        canChangeDisc: plugin.canChangeDisc,
        saveStates: plugin.saveStates ?? null,
        quickResume: plugin.quickResumeAccepted
          ? Object.freeze({
              screenshot: plugin.quickResumeScreenshot == null
                ? null
                : new Blob([new Uint8Array(plugin.quickResumeScreenshot)], { type: "image/png" }),
            })
          : null,
        quickResumeCache: plugin.quickResumeCache ?? null,
      });
      return launch;
    },
  });
  return Configuration;
}
