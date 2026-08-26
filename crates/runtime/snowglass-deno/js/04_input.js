import { op_input_configure } from "ext:core/ops";

const keyboardMouse = Object.freeze({
  id: "keyboard-mouse", kind: "keyboardMouse", name: "Keyboard + mouse", connected: true,
  assignedIndices: Object.freeze([]), buttons: Object.freeze([]), axes: Object.freeze([]),
});

function sourceId(selector) {
  if (selector?.kind === "keyboardMouse") return keyboardMouse.id;
  if (selector?.kind === "device") return String(selector.deviceId || "");
  return "";
}

function freezeSource(source, connected = true) {
  return Object.freeze({
    ...source,
    connected,
    assignedIndices: Object.freeze([...(source.assignedIndices || [])]),
    buttons: Object.freeze((source.buttons || []).map((button) => Object.freeze({ ...button }))),
    axes: Object.freeze((source.axes || []).map((axis) => Object.freeze({ ...axis }))),
    mapped: source.mapped && typeof source.mapped === "object"
      ? Object.freeze({ ...source.mapped })
      : undefined,
  });
}

function freezeConfiguration(configuration) {
  return Object.freeze({
    ...configuration,
    ports: Object.freeze([...(configuration?.ports || [])].map((port) => Object.freeze({ ...port }))),
    assignments: Object.freeze([...(configuration?.assignments || [])].map((assignment) =>
      Object.freeze({ ...assignment, source: Object.freeze({ ...assignment.source }) })
    )),
  });
}

class InputClient extends EventTarget {
  #configuration = freezeConfiguration({ revision: 0, ports: [], assignments: [] });
  #layouts = Object.freeze([]);
  #sources = Object.freeze([keyboardMouse]);
  #priority = "ui";
  #nextRequest = 1;
  #pendingRequests = new Map();
  #knownSources = new Map([[keyboardMouse.id, keyboardMouse]]);

  get configuration() { return this.#configuration; }
  get revision() { return Number(this.#configuration.revision) || 0; }
  get ports() { return this.#configuration.ports; }
  get assignments() { return this.#configuration.assignments; }
  get layouts() { return this.#layouts; }
  get sources() { return this.#sources; }
  get controllers() { return this.#connectedControllers(); }
  get priority() { return this.#priority; }

  getControllers() { return this.#connectedControllers(); }

  getGamepads() {
    return Object.freeze(this.#connectedControllers().map((source) => Object.freeze({
      id: source.name,
      sourceId: source.id,
      connected: true,
      mapping: "standard",
      buttons: Object.freeze(source.buttons.map((button) => Object.freeze({
        pressed: Boolean(button.pressed),
        touched: Boolean(button.pressed),
        value: button.pressed ? 1 : 0,
      }))),
      axes: Object.freeze(source.axes.map((axis) =>
        Math.max(-1, Math.min(1, Number(axis.value) / 32767))
      )),
    })));
  }

  getControllerLayout(id) {
    return this.#layouts.find((layout) => layout?.id === id) || null;
  }

  assign(assignments) {
    return this.configure({
      operations: [{
        type: "replaceAssignments",
        assignments: assignments.map((assignment) => ({
          index: Number(assignment.index),
          source: assignment.sourceId === keyboardMouse.id
            ? { kind: "keyboardMouse" }
            : { kind: "device", deviceId: String(assignment.sourceId) },
        })),
      }],
    });
  }

  configure(transaction) {
    if (!transaction || typeof transaction !== "object") {
      return Promise.reject(new TypeError("input transaction must be an object"));
    }
    const id = this.#nextRequest++;
    const wire = { ...transaction, expectedRevision: transaction.expectedRevision ?? this.revision };
    return new Promise((resolve, reject) => {
      this.#pendingRequests.set(id, { resolve, reject });
      op_input_configure(id, wire);
    });
  }

  publishConfiguration(configuration, layouts) {
    const previousAssignments = JSON.stringify(this.assignments);
    this.#configuration = freezeConfiguration(configuration);
    this.#layouts = Object.freeze([...(layouts || [])]);
    this.#refreshSources();
    this.dispatchEvent(new CustomEvent("configurationchange", { detail: this.#configuration }));
    if (previousAssignments !== JSON.stringify(this.assignments)) {
      this.dispatchEvent(new CustomEvent("assignmentchange", { detail: this.assignments }));
    }
  }

  publishState(sources, priority, controls) {
    const incoming = new Map();
    for (const value of sources || []) {
      const source = freezeSource(value, true);
      incoming.set(source.id, source);
      const previous = this.#knownSources.get(source.id);
      this.#knownSources.set(source.id, source);
      if (!previous?.connected) {
        this.dispatchEvent(new CustomEvent("controllerconnected", { detail: source }));
      }
    }
    for (const [id, previous] of this.#knownSources) {
      if (id === keyboardMouse.id || incoming.has(id) || !previous.connected) continue;
      const disconnected = freezeSource({ ...previous, buttons: [], axes: [] }, false);
      this.#knownSources.set(id, disconnected);
      this.dispatchEvent(new CustomEvent("controllerdisconnected", { detail: disconnected }));
    }
    this.#refreshSources();
    const nextPriority = priority === "game" ? "game" : "ui";
    if (nextPriority !== this.#priority) {
      this.#priority = nextPriority;
      this.dispatchEvent(new CustomEvent("prioritychange", { detail: this.#priority }));
    }
    this.dispatchEvent(new CustomEvent("inputstatechange", { detail: this.#sources }));
    for (const control of controls || []) {
      this.dispatchEvent(new CustomEvent("controllerinput", {
        detail: Object.freeze({ ...control }),
      }));
    }
  }

  settleConfiguration(id, result) {
    const pending = this.#pendingRequests.get(Number(id));
    if (!pending) return;
    this.#pendingRequests.delete(Number(id));
    if (!result?.ok || !result.configuration) {
      pending.reject(new Error(result?.error || "Input configuration failed"));
      return;
    }
    this.#configuration = freezeConfiguration(result.configuration);
    pending.resolve(this.#configuration);
  }

  #connectedControllers() {
    return this.#sources.filter((source) => source.kind === "controller" && source.connected);
  }

  #keyboardSnapshot() {
    return freezeSource({
      ...keyboardMouse,
      assignedIndices: this.assignments
        .filter((assignment) => sourceId(assignment.source) === keyboardMouse.id)
        .map((assignment) => Number(assignment.index)),
    });
  }

  #refreshSources() {
    const assignedIds = new Set(this.assignments.map((assignment) => sourceId(assignment.source)));
    const keyboard = this.#keyboardSnapshot();
    this.#knownSources.set(keyboardMouse.id, keyboard);
    this.#sources = Object.freeze([
      keyboard,
      ...[...this.#knownSources.values()].filter((source) =>
        source.id !== keyboardMouse.id && (source.connected || assignedIds.has(source.id))
      ),
    ]);
  }
}

export function createInput() {
  const client = new InputClient();
  return Object.freeze({
    snowglassInput: client,
    setConfiguration: (configuration, layouts) => client.publishConfiguration(configuration, layouts),
    setState: (sources, priority, controls) => client.publishState(sources, priority, controls),
    settleConfiguration: (id, result) => client.settleConfiguration(id, result),
  });
}
