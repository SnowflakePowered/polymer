import {
  op_configuration_configure,
  op_configuration_describe,
  op_configuration_prepare,
} from "ext:core/ops";

export function createConfiguration(scraping, pluginApi) {
  const sessions = new WeakMap();

  class ConfigurationState {
    #emulator;
    #identity;
    #layout;
    #package;
    #pending = Promise.resolve();
    #revision = 0;
    #values = {};

    constructor(packageValue, emulator) {
      this.#package = scraping.serialize(packageValue);
      this.#emulator = emulator.id;
      this.#identity = emulator.configuration;
      if (emulator.configuration === null) {
        this.#layout = null;
      } else {
        const { values, ...layout } = emulator.configuration;
        this.#layout = layout;
        this.#values = structuredClone(values);
      }
    }

    static async hydrate(packageValue, emulator) {
      const state = new ConfigurationState(packageValue, emulator);
      if (state.#layout === null) return state;
      const raw = await state.#read();
      if (raw === null) {
        await state.#commit(
          { layout: state.#layout, values: state.#values },
          state.#values,
          state.#revision,
        );
      } else {
        const configured = await state.#configure(raw);
        await state.#commit(configured, mergeValues(raw, configured.values), state.#revision);
      }
      return state;
    }

    configure(values = this.#values) {
      if (this.#layout === null) return Promise.resolve(this.snapshot());
      const revision = ++this.#revision;
      const pending = this.#configure(values).then(async (configured) => {
        if (revision !== this.#revision) return null;
        const retained = mergeValues(values, configured.values);
        return await this.#commit(configured, retained, revision) ? this.snapshot() : null;
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

    async #configure(values) {
      const configured = await op_configuration_configure(this.#package, this.#emulator, values);
      if (configured === null) throw new Error(`Plugin '${this.#emulator}' removed onConfigure`);
      if (configured.schema !== this.#identity.schema || configured.key !== this.#identity.key) {
        throw new Error(
          `Plugin configuration identity changed from ${this.#identity.schema}/${this.#identity.key} `
          + `to ${configured.schema}/${configured.key}`,
        );
      }
      const { values: settled, ...layout } = configured;
      return { layout, values: settled };
    }

    async #read() {
      try {
        const stored = await localStorage.getItem(this.#storageKey());
        if (stored === null) return null;
        const value = JSON.parse(stored);
        return value && typeof value === "object" && !Array.isArray(value) ? value : null;
      } catch {
        return null;
      }
    }

    async #commit(configured, retained, revision) {
      if (revision !== this.#revision) return false;
      await localStorage.setItem(this.#storageKey(), JSON.stringify(retained));
      if (revision !== this.#revision) return false;
      this.#layout = configured.layout;
      this.#values = retained;
      return true;
    }

    #storageKey() {
      return `snowglass.configuration.v1:${this.#emulator}:`
        + `${encodeURIComponent(this.#identity.schema)}:${encodeURIComponent(this.#identity.key)}`;
    }
  }

  function mergeValues(retained, settled) {
    const merged = structuredClone(retained);
    for (const [group, values] of Object.entries(settled)) {
      const previous = merged[group];
      merged[group] = {
        ...(previous && typeof previous === "object" && !Array.isArray(previous) ? previous : {}),
        ...values,
      };
    }
    return merged;
  }

  function stateFor(packageValue, emulator, operation) {
    const state = sessions.get(packageValue)?.get(String(emulator));
    if (state === undefined) {
      throw new DOMException(
        `Call configuration.describe() before ${operation}()`,
        "InvalidStateError",
      );
    }
    return state;
  }

  const Configuration = Object.freeze({
    async describe(packageValue, options = undefined) {
      const description = await op_configuration_describe(
        scraping.serialize(packageValue),
        options?.emulator == null ? null : String(options.emulator),
      );
      const configured = await Promise.all(description.candidates.map(async (emulator) => {
        const state = await ConfigurationState.hydrate(packageValue, emulator);
        Object.assign(emulator, state.snapshot());
        return [emulator.id, state];
      }));
      sessions.set(packageValue, new Map(configured));
      return description;
    },
    configure(packageValue, emulator, userConfiguration = undefined) {
      return stateFor(packageValue, emulator, "configure").configure(userConfiguration);
    },
    async prepare(packageValue, selection) {
      const configured = await stateFor(packageValue, selection.emulator, "prepare").settled();
      return op_configuration_prepare(scraping.serialize(packageValue), {
        ...selection,
        userConfiguration: configured.userConfiguration,
      });
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
