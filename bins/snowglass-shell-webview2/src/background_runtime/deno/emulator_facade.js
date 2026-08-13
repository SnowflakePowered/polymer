// Background Emulator lifecycle facade. The native op table remains the only
// authority for process, input, audio, and capture state.
globalThis.__snowglassCreateEmulatorFacade = (ops, HTMLTrackedSurfaceElement) => {
  const Emulator = new EventTarget();
  let activeInstance = null;

  function publishActiveInstance(instance) {
    if (activeInstance === instance) return;
    activeInstance = instance;
    Emulator.dispatchEvent(
      new CustomEvent("activeinstancechange", { detail: instance }),
    );
  }

  class EmulatorInstance {
    #listeners = new Map();
    #state = "starting";
    #error = null;

    constructor(id, requested) {
      this.id = id;
      this.requested = Object.freeze([...requested]);
      this.surfaces = Object.freeze([]);
      this.audio = Object.freeze({ available: false, volume: 1, muted: false });
    }

    get state() { return this.#state; }
    get error() { return this.#error; }
    #canControl() { return this.#state !== "closing" && this.#state !== "closed"; }

    on(eventName, callback) {
      if (typeof callback !== "function") throw new TypeError("event callback must be a function");
      const listeners = this.#listeners.get(String(eventName)) || new Set();
      listeners.add(callback);
      this.#listeners.set(String(eventName), listeners);
    }

    off(eventName, callback) {
      const listeners = this.#listeners.get(String(eventName));
      if (!listeners) return;
      listeners.delete(callback);
      if (listeners.size === 0) this.#listeners.delete(String(eventName));
    }

    send(name, args) {
      if (this.#canControl()) ops.op_emulator_message(this.id, String(name), args ?? null);
    }

    setVolume(volume) {
      volume = Number(volume);
      if (!Number.isFinite(volume)) throw new TypeError("Emulator volume must be a finite number");
      if (this.#canControl()) {
        ops.op_emulator_set_volume(this.id, Math.max(0, Math.min(1, volume)));
      }
    }

    setMuted(muted) {
      if (this.#canControl()) ops.op_emulator_set_muted(this.id, Boolean(muted));
    }
    activateInput() {
      if (this.#canControl()) ops.op_emulator_input(this.id, true);
    }
    deactivateInput() {
      if (this.#canControl()) ops.op_emulator_input(this.id, false);
    }
    close() {
      if (!this.#canControl()) return;
      this.#state = "closing";
      ops.op_emulator_close(this.id);
    }

    __nativeEvent(eventName, payload) {
      if (eventName === "start") {
        this.#state = "starting";
      } else if (eventName === "ready") {
        this.#state = "ready";
      } else if (eventName === "surfacesChanged") {
        const connected = new Map(
          Array.isArray(payload) ? payload.map((surface) => [surface.index, surface]) : [],
        );
        this.surfaces = Object.freeze(
          this.requested.filter((surface) => connected.has(surface.index)),
        );
        for (const surface of this.requested) {
          const native = connected.get(surface.index);
          if (!native) continue;
          surface.attach.dataset.connection = String(native.connection);
          surface.attach.dataset.index = String(native.index);
          surface.attach.dataset.preferredWidth = String(native.width);
          surface.attach.dataset.preferredHeight = String(native.height);
          surface.attach.style.setProperty("--preferred-width", `${native.width}px`);
          surface.attach.style.setProperty("--preferred-height", `${native.height}px`);
          surface.attach.style.setProperty(
            "--preferred-aspect-ratio",
            native.width && native.height ? String(native.width / native.height) : "auto",
          );
        }
      } else if (eventName === "audioChanged") {
        this.audio = Object.freeze({
          available: Boolean(payload?.available),
          volume: Math.max(0, Math.min(1, Number(payload?.volume) || 0)),
          muted: Boolean(payload?.muted),
        });
      } else if (eventName === "error") {
        this.#error = new Error(payload?.message || "Emulator instance failed");
        this.#state = "error";
      } else if (eventName === "closed") {
        this.#state = "closed";
        this.surfaces = Object.freeze([]);
        if (activeInstance === this) publishActiveInstance(null);
      }
      for (const callback of this.#listeners.get(eventName) || []) {
        try { callback(payload); }
        catch (error) { console.error(`EmulatorInstance '${eventName}' handler failed`, error); }
      }
    }
  }

  function launch(options) {
    if (activeInstance) return null;
    try {
      if (!options || !Array.isArray(options.surfaces)) {
        throw new TypeError("Emulator launch requires tracked surfaces");
      }
      for (const surface of options.surfaces) {
        if (!(surface.attach instanceof HTMLTrackedSurfaceElement)) {
          throw new TypeError(
            `surface ${surface.index} must attach to an HTMLTrackedSurfaceElement`,
          );
        }
        if (!surface.attach.isConnected) {
          throw new TypeError(`surface ${surface.index} attachment is not connected`);
        }
      }
      const wire = {
        command: options.command,
        arguments: [...(options.arguments || [])],
        isolated: Boolean(options.isolated),
        hook: [...options.hook],
        surfaces: options.surfaces.map((surface) => ({
          index: surface.index,
          rotation: surface.rotation || 0,
          selector: surface.selector,
          attach: surface.attach.__id,
          webrtc: Boolean(surface.webrtc),
        })),
        gamepadMapping: (options.gamepadMapping || []).map((mapping) => ({
          ...mapping,
          mapping: { ...(mapping.mapping || {}) },
        })),
        machine: options.machine,
        webrtcBind: options.webrtcBind,
        generatedConfiguration: options.generatedConfiguration,
      };
      const instance = new EmulatorInstance(ops.op_emulator_launch(wire), options.surfaces);
      publishActiveInstance(instance);
      return instance;
    } catch (error) {
      console.error("Emulator launch failed", error);
      return null;
    }
  }

  Object.defineProperties(Emulator, {
    activeInstance: { get: () => activeInstance, configurable: false },
    launch: { value: launch, writable: false, configurable: false },
  });

  return Object.freeze({
    Emulator,
    dispatch(event) {
      if (activeInstance?.id === Number(event.instanceId)) {
        activeInstance.__nativeEvent(event.eventName, event.payload);
      }
    },
  });
};
