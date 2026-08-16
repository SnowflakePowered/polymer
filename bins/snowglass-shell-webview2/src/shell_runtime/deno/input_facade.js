// Promise-based shell input facade. Installed before the DOM bootstrap
// and instantiated with the runtime's native op table.
globalThis.__snowglassCreateInputFacade = (ops) => {
  const snowglassInput = new EventTarget();
  let inputConfiguration = Object.freeze({ revision: 0, ports: Object.freeze([]) });
  let inputLayouts = Object.freeze([]);
  let inputControllers = Object.freeze([]);
  let inputPriority = "ui";
  let inputTimestamp = 0;
  let nextInputRequest = 1;
  const pendingInputRequests = new Map();

  Object.defineProperties(snowglassInput, {
    configuration: { get: () => inputConfiguration },
    revision: { get: () => Number(inputConfiguration.revision) || 0 },
    ports: { get: () => inputConfiguration.ports || Object.freeze([]) },
    layouts: { get: () => inputLayouts },
    controllers: { get: () => inputControllers },
    priority: { get: () => inputPriority },
    getGamepads: {
      value: () => {
        const devices = inputControllers.filter(
          (device) => device && typeof device === "object",
        );
        const result = Array(
          devices.reduce(
            (length, device) => Math.max(length, Number(device.index) + 1),
            0,
          ),
        ).fill(null);
        for (const device of devices) {
          const visible = (mapped) => inputPriority === "ui" || !mapped;
          result[device.index] = Object.freeze({
            index: device.index,
            deviceId: device.deviceId,
            id: `${device.name} (${device.guid})`,
            connected: true,
            mapping: "standard",
            timestamp: inputTimestamp,
            assignedPorts: Object.freeze([...(device.assignedPorts || [])]),
            buttons: Object.freeze(
              device.buttons.map((button) => {
                const pressed = visible(button.mapped) && button.pressed;
                return Object.freeze({
                  pressed,
                  touched: pressed,
                  value: pressed ? 1 : 0,
                });
              }),
            ),
            axes: Object.freeze(
              device.axes.map((axis) =>
                visible(axis.mapped)
                  ? Math.max(-1, Math.min(1, axis.value / 32767))
                  : 0,
              ),
            ),
            metadata: Object.freeze({
              guid: device.guid,
              vendor: device.vendor,
              product: device.product,
            }),
          });
        }
        return Object.freeze(result);
      },
    },
    getControllerLayout: {
      value: (id) => inputLayouts.find((layout) => layout?.id === id) || null,
    },
    configure: {
      value: (transaction) => {
        if (!transaction || typeof transaction !== "object") {
          return Promise.reject(new TypeError("input transaction must be an object"));
        }
        const id = nextInputRequest++;
        const wire = {
          ...transaction,
          expectedRevision:
            transaction.expectedRevision ?? inputConfiguration.revision ?? 0,
        };
        return new Promise((resolve, reject) => {
          pendingInputRequests.set(id, { resolve, reject });
          ops.op_input_configure(id, wire);
        });
      },
    },
  });

  return Object.freeze({
    snowglassInput,
    setConfiguration(configuration, layouts) {
      inputConfiguration = Object.freeze({ ...configuration });
      inputLayouts = Object.freeze([...(layouts || [])]);
      snowglassInput.dispatchEvent(
        new CustomEvent("configurationchange", { detail: inputConfiguration }),
      );
    },
    setState(controllers, priority) {
      inputControllers = Object.freeze([...(controllers || [])]);
      inputPriority = priority === "game" ? "game" : "ui";
      inputTimestamp = performance.now();
      snowglassInput.dispatchEvent(
        new CustomEvent("inputstatechange", { detail: inputControllers }),
      );
    },
    settleConfiguration(id, result) {
      const pending = pendingInputRequests.get(Number(id));
      if (!pending) return;
      pendingInputRequests.delete(Number(id));
      if (!result?.ok || !result.configuration) {
        pending.reject(new Error(result?.error || "Input configuration failed"));
        return;
      }
      inputConfiguration = Object.freeze({ ...result.configuration });
      pending.resolve(inputConfiguration);
    },
  });
};
