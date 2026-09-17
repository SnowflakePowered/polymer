import { op_firmware_list, op_firmware_open } from "ext:core/ops";

const store = Object.freeze({
  async list() {
    return Object.freeze((await op_firmware_list()).map(item => Object.freeze({
      ...item, names: Object.freeze(item.names),
    })));
  },
  async open(lookup) {
    if (typeof lookup === "string") return store.open({ name: lookup });
    if (lookup === null || typeof lookup !== "object" || Array.isArray(lookup)) {
      throw new TypeError("firmware lookup must be a name or lookup object");
    }
    const key = Object.keys(lookup).find(key =>
      key === "name" || key === "md5" || key === "sha1" || key === "sha256"
    );
    if (key === undefined || typeof lookup[key] !== "string" || lookup[key].length === 0) {
      throw new TypeError("firmware lookup requires a nonempty name or hash");
    }
    return __polymerFilesystem.fileResource(await op_firmware_open({ [key]: lookup[key] }));
  },
});

Object.defineProperty(globalThis, "__polymerFirmware", { value: Object.freeze({ store }) });
