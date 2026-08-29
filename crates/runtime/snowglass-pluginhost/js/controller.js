import {
  op_controller_change_flush,
  op_virtual_gamepad_set_connected,
} from "ext:core/ops";

function gamepad(index, initiallyConnected) {
  let connected = Boolean(initiallyConnected);
  return Object.freeze({
    index,
    get connected() {
      return connected;
    },
    connect() {
      connected = true;
      op_virtual_gamepad_set_connected(index, true);
    },
    disconnect() {
      connected = false;
      op_virtual_gamepad_set_connected(index, false);
    },
  });
}

function context(controllerMappings, gamepadStates, configuration, filesystem) {
  const gamepads = Object.freeze(
    gamepadStates.map((connected, index) => gamepad(index, connected)),
  );
  return Object.freeze({
    controllerMappings: Object.freeze(controllerMappings),
    gamepads,
    configuration,
    filesystem,
    flush() {
      __snowglassFilesystem.flush();
      op_controller_change_flush();
    },
  });
}

Object.defineProperty(globalThis, "__snowglassControllerChange", {
  value: Object.freeze({ context }),
  configurable: false,
  enumerable: false,
  writable: false,
});
