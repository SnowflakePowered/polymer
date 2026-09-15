import { op_firmware_input, op_firmware_result, op_firmware_list, op_firmware_open } from "ext:core/ops";

const store = Object.freeze({
  async list() {
    return Object.freeze((await op_firmware_list()).map(item => Object.freeze(item)));
  },
  async open(type, label) {
    if (typeof type !== "string" || typeof label !== "string") throw new TypeError("firmware type and label must be strings");
    return __polymerFilesystem.fileResource(await op_firmware_open(type, label));
  },
});

async function check(entrypoint) {
  const namespace = await import(entrypoint);
  const receiver = namespace.onCheckFirmware === undefined ? namespace.default : namespace;
  const callback = receiver?.onCheckFirmware;
  if (callback === undefined) return;
  if (typeof callback !== "function") throw new TypeError("onCheckFirmware must be a function");
  const result = await Reflect.apply(callback, receiver, [__polymerFilesystem.fileResource(op_firmware_input())]);
  if (result == null) return;
  if (!Array.isArray(result)) throw new TypeError("onCheckFirmware must return an array or no match");
  for (const item of result) {
    if (item === null || typeof item !== "object") throw new TypeError("invalid firmware descriptor");
    if (item.contents !== undefined && !(item.contents instanceof Uint8Array)) {
      throw new TypeError("firmware contents must be a Uint8Array");
    }
    op_firmware_result({ platform: item.platform, type: item.type, label: item.label }, item.contents ?? null);
  }
}

Object.defineProperty(globalThis, "__polymerFirmware", { value: Object.freeze({ store, check }) });
