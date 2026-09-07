import {
  op_emulator_close,
  op_emulator_input,
  op_emulator_launch,
  op_emulator_message,
  op_emulator_pause_state,
  op_emulator_save_state,
  op_emulator_resume_state,
  op_emulator_state_screenshot,
  op_emulator_set_muted,
  op_emulator_set_volume,
} from "ext:core/ops";
import { attachmentFor } from "./00_infra.js";

// Deno owns launch object identity and lifecycle. Native code owns process,
// input, audio, capture, and compositor state behind the op boundary.
export function createEmulator(webrtc) {
  const Emulator = new EventTarget();
  let activeLaunch = null;
  let nextLaunchId = 0;

  function deferred() {
    let resolve;
    let reject;
    const promise = new Promise((accept, decline) => {
      resolve = accept;
      reject = decline;
    });
    return { promise, resolve, reject };
  }

  function publishActiveLaunch(launch) {
    if (activeLaunch === launch) return;
    activeLaunch = launch;
    Emulator.dispatchEvent(new CustomEvent("activelaunchchange", { detail: launch }));
  }

  function validate(options) {
    if (!options || !Array.isArray(options.surfaces)) {
      throw new TypeError("Emulator launch requires tracked surfaces");
    }
    for (const surface of options.surfaces) {
      attachmentFor(surface.attach);
      if (!surface.attach.isConnected) {
        throw new TypeError(`surface ${surface.index} attachment is not connected`);
      }
    }
  }

  function wireOptions(options) {
    return {
      command: options.command,
      arguments: [...(options.arguments || [])],
      isolated: Boolean(options.isolated),
      hooks: {
        graphics: [...options.hooks.graphics],
        input: [...options.hooks.input],
      },
      surfaces: options.surfaces.map((surface) => ({
        index: surface.index,
        rotation: surface.rotation || 0,
        // Copy the extent out of whatever the shell handed us: a framework
        // proxy over an array is not an array to the op boundary.
        resolution: Array.isArray(surface.resolution) ? [...surface.resolution] : surface.resolution,
        selector: surface.selector,
        attach: attachmentFor(surface.attach),
      })),
      controllerTopology: {
        mappings: (options.controllerTopology?.mappings || []).map((mapping) => ({
          ...mapping,
          projectionMode: mapping.projectionMode ?? "position",
          mapping: { ...(mapping.mapping || {}) },
        })),
      },
      machine: options.machine,
      preparedFilesystem: options.preparedFilesystem,
      controllerSession: options.controllerSession,
      saveStates: options.saveStates ?? null,
      quickResumeCache: options.quickResumeCache ?? null,
    };
  }

  class LaunchInstance extends EventTarget {
    #state = "pending";
    #error = null;
    #ready = deferred();
    #closed = deferred();
    #nativeStarted = false;
    #closeRequested = false;
    #readySettled = false;
    #closedSettled = false;

    constructor(id, options) {
      super();
      this.id = id;
      this.request = Object.freeze(options);
      this.quickResume = options.quickResume ?? null;
      this.requested = Object.freeze([...options.surfaces]);
      this.surfaces = Object.freeze([]);
      this.audio = Object.freeze({ available: false, volume: 1, muted: false });
      this.saveStates = Object.freeze([]);
      this.ready = this.#ready.promise;
      this.closed = this.#closed.promise;
    }

    get state() { return this.#state; }
    get error() { return this.#error; }
    #canControl() { return this.#state !== "closing" && this.#state !== "closed"; }

    #publish(type, detail = undefined) {
      this.dispatchEvent(new CustomEvent(type, { detail }));
    }

    #rejectReady(error) {
      if (this.#readySettled) return;
      this.#readySettled = true;
      this.#ready.reject(error);
    }

    #resolveReady() {
      if (this.#readySettled) return;
      this.#readySettled = true;
      this.#ready.resolve();
    }

    #resolveClosed() {
      if (this.#closedSettled) return;
      this.#closedSettled = true;
      this.#closed.resolve();
    }

    async __start(wire) {
      try {
        await op_emulator_launch(this.id, wire);
        this.#nativeStarted = true;
        if (this.#state === "pending") this.#state = "starting";
        if (this.#closeRequested) {
          op_emulator_close(this.id);
          return;
        }
        const targets = this.request.surfaces
          .filter((surface) => surface.webrtc)
          .map((surface) => surface.index);
        if (targets.length > 0) {
          await webrtc.startForInstance(this, {
            bind: this.request.webrtcBind,
            targets,
          });
        }
      } catch (value) {
        const error = value instanceof Error ? value : new Error(String(value));
        if (this.#nativeStarted) op_emulator_close(this.id);
        this.__fail(error);
      }
    }

    __fail(error) {
      if (this.#state === "closed") return;
      this.#error = error;
      this.#state = "error";
      this.#rejectReady(error);
      this.#publish("error", error);
      this.__nativeEvent("closed", null);
    }

    send(name, args) {
      if (this.#canControl()) op_emulator_message(this.id, String(name), args ?? null);
    }

    setVolume(volume) {
      volume = Number(volume);
      if (!Number.isFinite(volume)) throw new TypeError("Emulator volume must be a finite number");
      if (this.#canControl()) op_emulator_set_volume(this.id, Math.max(0, Math.min(1, volume)));
    }

    setMuted(muted) {
      if (this.#canControl()) op_emulator_set_muted(this.id, Boolean(muted));
    }

    activateInput() {
      if (this.#canControl()) op_emulator_input(this.id, true);
    }

    deactivateInput() {
      if (this.#canControl()) op_emulator_input(this.id, false);
    }

    async setPauseState(pauseState, timeoutMs = 30_000) {
      if (pauseState !== "pause" && pauseState !== "resume") {
        throw new TypeError("pause state must be 'pause' or 'resume'");
      }
      if (!Number.isInteger(timeoutMs) || timeoutMs <= 0 || timeoutMs > 0xffff_ffff) {
        throw new TypeError("pause/resume timeout must be a positive 32-bit integer");
      }
      if (this.#canControl()) {
        await op_emulator_pause_state(this.id, pauseState === "pause", timeoutMs);
      }
    }

    #validateStateTimeout(timeoutMs) {
      if (!Number.isInteger(timeoutMs) || timeoutMs <= 0 || timeoutMs > 0xffff_ffff) {
        throw new TypeError("save-state timeout must be a positive 32-bit integer");
      }
    }

    #saveStateRecord(metadata) {
      const launch = this;
      let screenshot;
      return Object.freeze({
        slot: metadata.slot,
        timestamp: new Date(metadata.timestamp),
        async screenshot() {
          if (!metadata.hasScreenshot) return null;
          if (screenshot === undefined) {
            screenshot = (async () => {
              const bytes = await op_emulator_state_screenshot(launch.id, metadata.recordId);
              return bytes == null ? null : new Blob([bytes], { type: "image/png" });
            })();
          }
          return screenshot;
        },
      });
    }

    async saveState(timeoutMs = 30_000) {
      this.#validateStateTimeout(timeoutMs);
      if (!this.#canControl()) throw new DOMException("The launch is closing", "InvalidStateError");
      const state = this.#saveStateRecord(await op_emulator_save_state(this.id, timeoutMs));
      this.saveStates = Object.freeze([
        state,
        ...this.saveStates.filter(candidate => candidate.slot !== state.slot),
      ]);
      this.#publish("statesave", state);
      return state;
    }

    async resumeState(slot, timeoutMs = 30_000) {
      if (!Number.isInteger(slot) || slot < 0 || slot > 0xffff_ffff) {
        throw new TypeError("save-state slot must be a non-negative 32-bit integer");
      }
      this.#validateStateTimeout(timeoutMs);
      if (!this.#canControl()) throw new DOMException("The launch is closing", "InvalidStateError");
      const state = this.#saveStateRecord(await op_emulator_resume_state(this.id, slot, timeoutMs));
      this.#publish("stateresume", state);
      return state;
    }

    close() {
      if (!this.#canControl()) return;
      this.#closeRequested = true;
      this.#state = "closing";
      this.#rejectReady(new DOMException("The launch was closed", "AbortError"));
      if (this.#nativeStarted) op_emulator_close(this.id);
    }

    __nativeEvent(eventName, payload) {
      if (eventName === "start") {
        if (this.#state === "pending") this.#state = "starting";
      } else if (eventName === "ready") {
        if (this.#closeRequested) {
          if (this.#nativeStarted) op_emulator_close(this.id);
          return;
        }
        this.#state = "ready";
        this.#resolveReady();
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
        const error = new Error(payload?.message || "Emulator launch failed");
        this.#error = error;
        this.#state = "error";
        this.#rejectReady(error);
        payload = error;
      } else if (eventName === "closed") {
        if (!this.#readySettled) {
          this.#rejectReady(this.#error || new Error("Emulator closed before becoming ready"));
        }
        this.#state = "closed";
        this.saveStates = Object.freeze([]);
        this.surfaces = Object.freeze([]);
        webrtc.instanceClosed(this);
        if (activeLaunch === this) publishActiveLaunch(null);
        this.#resolveClosed();
      }
      this.#publish(eventName, payload);
    }
  }

  function launch(options) {
    if (activeLaunch) throw new DOMException("another emulator launch is active", "InvalidStateError");
    validate(options);
    const id = nextLaunchId = (nextLaunchId + 1) >>> 0 || 1;
    const instance = new LaunchInstance(id, options);
    publishActiveLaunch(instance);
    void instance.__start(wireOptions(options));
    return instance;
  }

  Object.defineProperties(Emulator, {
    LaunchInstance: { value: LaunchInstance },
    activeLaunch: { get: () => activeLaunch },
    launch: { value: launch },
  });

  return Object.freeze({
    Emulator,
    dispatch(event) {
      if (activeLaunch?.id === Number(event.instanceId)) {
        activeLaunch.__nativeEvent(event.eventName, event.payload);
      }
    },
  });
}
