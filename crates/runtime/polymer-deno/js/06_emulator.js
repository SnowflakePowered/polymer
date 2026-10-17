import {
  op_emulator_close,
  op_emulator_input,
  op_emulator_launch,
  op_emulator_message,
  op_emulator_pause_state,
  op_emulator_change_disc,
  op_emulator_state_action,
  op_emulator_set_muted,
  op_emulator_set_volume,
} from "ext:core/ops";
import { attachmentFor } from "./00_infra.js";

// Deno owns launch object identity and lifecycle. Native code owns process,
// input, audio, capture, and compositor state behind the op boundary.
export function createEmulator() {
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
      workingDirectory: options.workingDirectory,
      env: (options.env || []).map(entry => [...entry]),
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
      launchable: options.launchable ?? null,
      preparedFilesystem: options.preparedFilesystem,
      controllerSession: options.controllerSession,
      launchSet: options.launchSet ?? [],
      initialDiscIndex: options.initialDiscIndex ?? 0,
      canChangeDisc: options.canChangeDisc === true,
      saveStates: options.saveStates ?? null,
      quickResumeCache: options.quickResumeCache ?? null,
    };
  }

  function validateStateTimeout(timeoutMs) {
    if (!Number.isInteger(timeoutMs) || timeoutMs <= 0 || timeoutMs > 0xffff_ffff) {
      throw new TypeError("save-state timeout must be a positive 32-bit integer");
    }
  }

  class SaveStateStore extends EventTarget {
    #capacity;
    #action;
    #entries = Object.freeze([]);
    #queue = Promise.resolve();

    constructor(capacity, action) {
      super();
      this.#capacity = capacity;
      this.#action = action;
    }

    get capacity() { return this.#capacity; }
    get entries() { return this.#entries; }

    get(slot) {
      return this.#entries.find((entry) => entry.slot === slot) ?? null;
    }

    #slotOf(target) {
      const slot = typeof target === "number" ? target : target?.slot;
      if (!Number.isInteger(slot) || slot < 0 || slot >= this.#capacity) {
        throw new RangeError(`save-state slot must be an integer in 0..${this.#capacity - 1}`);
      }
      return slot;
    }

    #nextSlot() {
      for (let slot = 0; slot < this.#capacity; slot++) {
        if (!this.get(slot)) return slot;
      }
      return this.#entries.at(-1).slot;
    }

    #enqueue(task) {
      const run = this.#queue.then(task);
      this.#queue = run.catch(() => {});
      return run;
    }

    #publish(type, detail = undefined) {
      this.dispatchEvent(new CustomEvent(type, { detail }));
    }

    save(options = {}) {
      const { label = null, createdAt = new Date(), screenshot = null, timeoutMs = 30_000 } = options;
      const slot = options.slot === undefined ? undefined : this.#slotOf(options.slot);
      validateStateTimeout(timeoutMs);
      if (label !== null && typeof label !== "string") throw new TypeError("save-state label must be a string");
      if (!(createdAt instanceof Date)) throw new TypeError("save-state createdAt must be a Date");
      if (screenshot !== null && !(screenshot.bytes instanceof Uint8Array && typeof screenshot.type === "string")) {
        throw new TypeError("save-state screenshot must be { bytes: Uint8Array, type: string }");
      }
      return this.#enqueue(async () => {
        const target = slot ?? this.#nextSlot();
        await this.#action(target, true, timeoutMs);
        const entry = Object.freeze({
          slot: target,
          createdAt: new Date(createdAt),
          label,
          screenshot: screenshot && Object.freeze({ bytes: screenshot.bytes, type: screenshot.type }),
        });
        this.#entries = Object.freeze([entry, ...this.#entries.filter((candidate) => candidate.slot !== target)]);
        this.#publish("save", entry);
        this.#publish("change", this.#entries);
        return entry;
      });
    }

    restore(target, options = {}) {
      const slot = this.#slotOf(target);
      const { timeoutMs = 30_000 } = options;
      validateStateTimeout(timeoutMs);
      return this.#enqueue(async () => {
        const entry = this.get(slot);
        if (!entry) throw new DOMException(`save-state slot ${slot} is empty`, "NotFoundError");
        await this.#action(slot, false, timeoutMs);
        this.#publish("restore", entry);
      });
    }

    delete(target) {
      const slot = this.#slotOf(target);
      if (!this.get(slot)) return;
      this.#entries = Object.freeze(this.#entries.filter((entry) => entry.slot !== slot));
      this.#publish("change", this.#entries);
    }

    __clear() {
      if (this.#entries.length === 0) return;
      this.#entries = Object.freeze([]);
      this.#publish("change", this.#entries);
    }
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
    #launchSet;
    #currentDiscIndex;

    constructor(id, options) {
      super();
      this.id = id;
      this.#launchSet = Object.freeze([...(options.launchSet ?? [])]);
      this.#currentDiscIndex = options.initialDiscIndex ?? 0;
      this.request = Object.freeze(options);
      this.quickResume = options.quickResume ?? null;
      this.requested = Object.freeze([...options.surfaces]);
      this.surfaces = Object.freeze([]);
      this.audio = Object.freeze({ available: false, volume: 1, muted: false });
      this.saveStates = options.saveStates
        ? new SaveStateStore(options.saveStates.max, async (slot, save, timeoutMs) => {
          if (!this.#canControl()) throw new DOMException("The launch is closing", "InvalidStateError");
          await op_emulator_state_action(this.id, slot, save, timeoutMs);
        })
        : null;
      this.ready = this.#ready.promise;
      this.closed = this.#closed.promise;
    }

    get state() { return this.#state; }
    get error() { return this.#error; }
    get currentDiscIndex() { return this.#currentDiscIndex; }
    get canChangeDisc() { return this.#launchSet.length > 1 && this.request.canChangeDisc === true; }
    getLaunchSet() { return this.#launchSet; }

    async changeDisc(index) {
      if (!Number.isInteger(index) || index < 0 || index >= this.#launchSet.length) {
        throw new RangeError("disc index is outside the launch set");
      }
      if (this.#state !== "ready") {
        throw new DOMException("The launch is not ready", "InvalidStateError");
      }
      if (!this.canChangeDisc) {
        throw new DOMException("The emulator does not support disc changes", "NotSupportedError");
      }
      await op_emulator_change_disc(this.id, index);
      this.#currentDiscIndex = index;
      if (this.#state === "ready") this.#publish("discchange", { index });
    }
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
        this.saveStates?.__clear();
        this.surfaces = Object.freeze([]);
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
