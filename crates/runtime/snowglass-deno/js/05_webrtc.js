import { op_webrtc_start, op_webrtc_stop } from "ext:core/ops";

// WebRTC control plane for the Snowglass realm. Native renderer code owns GPU
// frames, encoding, peer connections, and signaling; JavaScript owns the
// publication lifecycle and its association with emulator instances.
export function createWebRtc() {
  const WebRTC = new EventTarget();
  let activePublication = null;

  const publishActive = (publication) => {
    if (activePublication === publication) return;
    activePublication = publication;
    WebRTC.dispatchEvent(new CustomEvent("activepublicationchange", {
      detail: publication,
    }));
  };

  const normalizeTargets = (targets) => {
    if (!Array.isArray(targets) || targets.length === 0) {
      throw new TypeError("WebRTC publication requires at least one render target");
    }
    const normalized = targets.map((target) => Number(target));
    if (normalized.some((target) => !Number.isInteger(target) || target < 0 || target > 0xffffffff)) {
      throw new TypeError("WebRTC render targets must be unsigned 32-bit integers");
    }
    if (new Set(normalized).size !== normalized.length) {
      throw new TypeError("WebRTC render-target indices must be unique");
    }
    return Object.freeze(normalized);
  };

  class WebRtcPublication extends EventTarget {
    #state = "starting";
    #error = null;

    constructor(instance, bind, targets) {
      super();
      this.instance = instance;
      this.bind = bind;
      this.targets = targets;
    }

    get state() { return this.#state; }
    get error() { return this.#error; }

    __activate() {
      if (this.#state !== "starting") return false;
      this.#state = "active";
      this.dispatchEvent(new Event("statechange"));
      return true;
    }

    __fail(error) {
      this.#error = error instanceof Error ? error : new Error(String(error));
      this.#state = "error";
      this.dispatchEvent(new Event("statechange"));
      this.dispatchEvent(new CustomEvent("error", { detail: this.#error }));
    }

    __close() {
      if (this.#state === "closed") return;
      this.#state = "closed";
      this.dispatchEvent(new Event("statechange"));
      this.dispatchEvent(new Event("close"));
      if (activePublication === this) publishActive(null);
    }

    async close() {
      if (this.#state === "closed") return;
      if (this.#state === "closing") return;
      this.#state = "closing";
      this.dispatchEvent(new Event("statechange"));
      try {
        await op_webrtc_stop(this.instance.id);
      } finally {
        this.__close();
      }
    }
  }

  const startForInstance = async (instance, options = {}) => {
    if (!instance || !Number.isInteger(instance.id) || instance.id <= 0) {
      throw new TypeError("WebRTC publication requires an emulator instance");
    }
    if (instance.state === "closing" || instance.state === "closed" || instance.state === "error") {
      throw new DOMException("the emulator instance is not active", "InvalidStateError");
    }
    if (activePublication) {
      throw new DOMException("a WebRTC publication is already active", "InvalidStateError");
    }
    const bind = options.bind === undefined ? "127.0.0.1:8080" : String(options.bind);
    const targets = normalizeTargets(options.targets);
    const publication = new WebRtcPublication(instance, bind, targets);
    publishActive(publication);
    try {
      await op_webrtc_start(instance.id, bind, targets);
      if (!publication.__activate()) {
        throw new DOMException("the emulator instance closed while starting WebRTC", "InvalidStateError");
      }
      return publication;
    } catch (error) {
      if (publication.state !== "closed") {
        publication.__fail(error);
        publication.__close();
      }
      throw error;
    }
  };

  Object.defineProperties(WebRTC, {
    activePublication: { get: () => activePublication, configurable: false },
    start: {
      value: (options) => startForInstance(options?.instance, options),
      writable: false,
      configurable: false,
    },
  });

  return Object.freeze({
    WebRTC,
    startForInstance,
    instanceClosed(instance) {
      if (activePublication?.instance === instance) activePublication.__close();
    },
  });
}
