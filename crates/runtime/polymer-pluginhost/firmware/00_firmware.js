import { op_firmware_list, op_firmware_open } from "ext:core/ops";

const store = Object.freeze({
  async list() {
    return Object.freeze((await op_firmware_list()).map(item => Object.freeze(item)));
  },
  async open(type, label) {
    if (typeof type !== "string" || typeof label !== "string") throw new TypeError("firmware type and label must be strings");
    return __polymerFilesystem.fileResource(await op_firmware_open(type, label));
  },
});

Object.defineProperty(globalThis, "__polymerFirmware", { value: Object.freeze({ store }) });
