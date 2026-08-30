import { op_input_api_command } from "ext:core/ops";

const freezeDevice = (device) => device ? Object.freeze({ ...device }) : null;

function freezePlayers(players) {
  return Object.freeze((players || []).map((player) => Object.freeze({
    index: Number(player.index),
    controller: freezeDevice(player.controller),
  })));
}

function freezeState(state) {
  return Object.freeze({
    availability: Object.freeze({
      osBle: state?.availability?.osBle || "unknown",
      winUsb: state?.availability?.winUsb || "unknown",
    }),
    devices: Object.freeze((state?.devices || []).map(freezeDevice)),
    players: freezePlayers(state?.players),
    activeConnectionSession: state?.activeConnectionSession || null,
    layouts: Object.freeze([...(state?.layouts || [])]),
  });
}

class ConnectionSession extends EventTarget {
  #controllers;
  #id;

  constructor(controllers, id) {
    super();
    this.#controllers = controllers;
    this.#id = id;
  }

  get id() { return this.#id; }
  get active() { return this.#controllers.isSessionActive(this.#id); }
  get discovery() { return this.active ? this.#controllers.sessionState.discovery : Object.freeze({ active: false }); }
  get candidates() { return this.active ? this.#controllers.sessionState.candidates : Object.freeze([]); }
  get players() { return this.active ? this.#controllers.players : Object.freeze([]); }

  connect(id) {
    return this.#controllers.sessionCommand("connect", this.#id, { id: String(id) });
  }

  finish() {
    return this.#controllers.sessionCommand("finish", this.#id);
  }
}

class ControllersClient extends EventTarget {
  #state;
  #session = null;
  #sessionState = Object.freeze({ discovery: Object.freeze({ active: false }), candidates: Object.freeze([]) });

  constructor(initial) {
    super();
    this.#state = freezeState(initial);
    this.#syncSession();
  }

  get availability() { return this.#state.availability; }
  get devices() { return this.#state.devices; }
  get connected() { return Object.freeze(this.devices.filter((device) => device.connected)); }
  get players() { return this.#state.players; }
  get layouts() { return this.#state.layouts; }
  get activeConnectionSession() { return this.#state.activeConnectionSession ? this.#session : null; }
  get sessionState() { return this.#sessionState; }
  isSessionActive(id) { return this.#state.activeConnectionSession?.id === id; }

  async startConnectionSession() {
    if (this.activeConnectionSession) throw new Error("a controller connection session is already active");
    this.publish(await op_input_api_command({ type: "controllers.session.start" }));
    const session = this.activeConnectionSession;
    if (!session) throw new Error("the controller connection session did not start");
    return session;
  }

  disconnect(id) { return this.#stateCommand({ type: "controllers.disconnect", id: String(id) }); }
  forget(id) { return this.#stateCommand({ type: "controllers.forget", id: String(id) }); }
  getMapping(id, layoutId) {
    return op_input_api_command({ type: "controllers.mapping.get", id: String(id), layoutId: String(layoutId) });
  }
  setMapping(id, layoutId, mapping) {
    return op_input_api_command({
      type: "controllers.mapping.set", id: String(id), layoutId: String(layoutId), mapping,
    });
  }
  resetMapping(id, layoutId) {
    return op_input_api_command({ type: "controllers.mapping.reset", id: String(id), layoutId: String(layoutId) });
  }

  async sessionCommand(action, sessionId, detail = {}) {
    const state = await op_input_api_command({
      type: `controllers.session.${action}`,
      sessionId,
      ...detail,
    });
    this.publish(state);
  }

  async #stateCommand(command) {
    this.publish(await op_input_api_command(command));
  }

  publish(state) {
    const previous = this.#state;
    const previousSession = this.#session;
    this.#state = freezeState(state?.controllers || state);
    this.#syncSession();
    if (JSON.stringify(previous.availability) !== JSON.stringify(this.availability)) {
      this.dispatchEvent(new CustomEvent("availabilitychange", { detail: this.availability }));
    }
    if (JSON.stringify(previous.devices) !== JSON.stringify(this.devices)) {
      this.dispatchEvent(new CustomEvent("devicechange", { detail: this.devices }));
    }
    const previousDevices = new Map(previous.devices.map((device) => [device.id, device]));
    for (const device of this.devices) {
      if (device.connected && !previousDevices.get(device.id)?.connected) {
        this.dispatchEvent(new CustomEvent("controllerconnected", { detail: device }));
      }
    }
    for (const device of previous.devices) {
      if (device.connected && !this.devices.find((candidate) => candidate.id === device.id)?.connected) {
        this.dispatchEvent(new CustomEvent("controllerdisconnected", { detail: device }));
      }
    }
    if (JSON.stringify(previous.players) !== JSON.stringify(this.players)) {
      this.dispatchEvent(new CustomEvent("playerchange", { detail: this.players }));
      if (this.#session?.active) {
        const prior = new Map(previous.players.map((player) => [player.index, player.controller]));
        for (const player of this.players) {
          const replaced = prior.get(player.index);
          if (replaced?.id !== player.controller?.id && replaced) {
            this.#session.dispatchEvent(new CustomEvent("controllerdisconnected", {
              detail: Object.freeze({ index: player.index, controller: replaced }),
            }));
          }
          if (replaced?.id !== player.controller?.id && player.controller) {
            this.#session.dispatchEvent(new CustomEvent("controllerconnected", {
              detail: Object.freeze({ index: player.index, controller: player.controller }),
            }));
          }
        }
        this.#session.dispatchEvent(new CustomEvent("playerchange", { detail: this.players }));
      }
    }
    if (previousSession && previousSession !== this.#session) {
      previousSession.dispatchEvent(new CustomEvent("close"));
    }
    if (this.#session?.active) {
      this.#session.dispatchEvent(new CustomEvent("change", { detail: this.#session }));
    }
    this.dispatchEvent(new CustomEvent("statechange", { detail: this.#state }));
  }

  #syncSession() {
    const state = this.#state.activeConnectionSession;
    this.#sessionState = Object.freeze({
      discovery: Object.freeze({ active: Boolean(state?.discovery?.active) }),
      candidates: Object.freeze((state?.candidates || []).map(freezeDevice)),
    });
    if (!state) {
      this.#session = null;
    } else if (this.#session?.id !== state.id) {
      this.#session = new ConnectionSession(this, String(state.id));
    }
  }
}

export function createControllers(initial) {
  const controllers = new ControllersClient(initial);
  return Object.freeze({ controllers, setState: (state) => controllers.publish(state) });
}
