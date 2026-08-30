import { op_input_command, op_input_devices, op_input_get } from "ext:core/ops";

const calibrationPrefix = "snowglass.input.calibration.";

class CalibrationCapture {
  #deviceId;

  constructor(deviceId) {
    this.#deviceId = deviceId;
  }

  get progress() {
    return op_input_command({ type: "calibrationProgress", deviceId: this.#deviceId }).progress;
  }

  finish() {
    const value = op_input_command({ type: "finishCalibration", deviceId: this.#deviceId });
    void this.#store(value);
    return value;
  }

  cancel() {
    op_input_command({ type: "cancelCalibration", deviceId: this.#deviceId });
  }

  async #store(value) {
    try {
      await localStorage.setItem(calibrationPrefix + this.#deviceId, JSON.stringify(value));
    } catch (error) {
      console.warn(`Unable to persist input calibration for ${this.#deviceId}`, error);
    }
  }
}

class InputClient extends EventTarget {
  constructor() {
    super();
    for (const device of this.devices) {
      if (device.connected) void this.#restoreCalibration(device.id);
    }
  }

  get devices() {
    return Object.freeze(op_input_devices());
  }

  get(index) {
    return op_input_get(index);
  }

  rumble(index, options) {
    op_input_command({ type: "rumble", index, ...options });
  }

  recenter(index) {
    op_input_command({ type: "recenter", index });
  }

  calibrate(deviceId, calibration) {
    const value = op_input_command({ type: "calibrate", deviceId, calibration });
    void this.#storeCalibration(deviceId, value);
    return value;
  }

  getCalibration(deviceId) {
    return op_input_command({ type: "getCalibration", deviceId });
  }

  resetCalibration(deviceId) {
    const value = op_input_command({ type: "resetCalibration", deviceId });
    void this.#removeCalibration(deviceId);
    return value;
  }

  beginCalibration(deviceId, kind) {
    op_input_command({ type: "beginCalibration", deviceId, kind });
    return Object.freeze(new CalibrationCapture(deviceId));
  }

  configurePointer(deviceId, pointer) {
    return this.calibrate(deviceId, { pointer });
  }

  deviceConnected(device) {
    void this.#restoreCalibration(device.id);
    this.dispatchEvent(new CustomEvent("deviceconnected", { detail: Object.freeze(device) }));
  }

  deviceDisconnected(detail) {
    this.dispatchEvent(new CustomEvent("devicedisconnected", { detail: Object.freeze(detail) }));
  }

  deviceAssigned(detail) {
    this.dispatchEvent(new CustomEvent("deviceassigned", { detail: Object.freeze(detail) }));
  }

  inputChanged(detail) {
    this.dispatchEvent(new CustomEvent("input", { detail: Object.freeze(detail) }));
  }

  async #restoreCalibration(deviceId) {
    try {
      const stored = await localStorage.getItem(calibrationPrefix + deviceId);
      if (stored !== null) op_input_command({ type: "calibrate", deviceId, calibration: JSON.parse(stored) });
    } catch (error) {
      console.warn(`Unable to restore input calibration for ${deviceId}`, error);
    }
  }

  async #storeCalibration(deviceId, calibration) {
    try {
      await localStorage.setItem(calibrationPrefix + deviceId, JSON.stringify(calibration));
    } catch (error) {
      console.warn(`Unable to persist input calibration for ${deviceId}`, error);
    }
  }

  async #removeCalibration(deviceId) {
    try {
      await localStorage.removeItem(calibrationPrefix + deviceId);
    } catch (error) {
      console.warn(`Unable to remove input calibration for ${deviceId}`, error);
    }
  }
}

export function createInput() {
  const client = new InputClient();
  return Object.freeze({ snowglassInput: client });
}
