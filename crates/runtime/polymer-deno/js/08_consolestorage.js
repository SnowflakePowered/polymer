import {
  op_consolestorage_create,
  op_consolestorage_delete,
  op_consolestorage_list,
  op_consolestorage_set_label,
  op_consolestorage_supports,
} from "ext:core/ops";

export function createConsoleStorage() {
  const storageData = new WeakMap();
  const storages = new Set();
  const deleted = new WeakSet();
  const internal = Symbol("Polymer.consolestorage.Storage");

  class Storage extends EventTarget {
    constructor(token, data) {
      super();
      if (token !== internal) throw new TypeError("Use Polymer.consolestorage.Storage.create()");
      storageData.set(this, Object.freeze({ ...data }));
      storages.add(this);
    }

    static async create(kind, platform, options = undefined) {
      if (kind === "battery") {
        throw new TypeError("Battery profiles are created automatically for each game");
      }
      if (!["nand", "saveRepo", "memoryCard"].includes(kind)) {
        throw new TypeError("Unknown console storage kind");
      }
      if (typeof platform !== "string" || platform.length === 0) {
        throw new TypeError("Console storage requires a platform id");
      }
      const label = options?.label === undefined ? null : String(options.label);
      return new Storage(internal, await op_consolestorage_create(kind, platform, label));
    }

    static async supports(kind, platform) {
      if (!["nand", "saveRepo", "memoryCard", "battery"].includes(kind)) {
        throw new TypeError("Unknown console storage kind");
      }
      if (typeof platform !== "string" || platform.length === 0) {
        throw new TypeError("Console storage requires a platform id");
      }
      return await op_consolestorage_supports(kind, platform);
    }

    static async list(options = undefined) {
      const storages = (await op_consolestorage_list()).map((data) => new Storage(internal, data));
      return options?.kind === undefined
        ? storages
        : storages.filter((storage) => storage.kind === options.kind);
    }

    get kind() { return storageData.get(this).kind; }
    get id() { return storageData.get(this).id; }
    get platform() { return storageData.get(this).platform; }
    get label() { return storageData.get(this).label; }
    get created() { return new Date(storageData.get(this).created); }
    get path() { return storageData.get(this).path; }

    async setLabel(label) {
      const data = storageData.get(this);
      const next = label === null || label === undefined ? null : String(label);
      return new Storage(internal, await op_consolestorage_set_label(data.kind, data.platform, data.id, next));
    }

    async delete() {
      const data = storageData.get(this);
      deleted.add(this);
      storages.delete(this);
      await op_consolestorage_delete(data.kind, data.platform, data.id);
    }

    toJSON() { return { ...storageData.get(this) }; }
  }

  function gameSaved(detail) {
    for (const storage of storages) {
      const data = storageData.get(storage);
      if (data.kind === detail.kind && data.platform === detail.platform && data.id === detail.id) {
        storage.dispatchEvent(new Event("gamesaved"));
      }
    }
  }

  const ConsoleStorage = Object.freeze({ Storage });
  return Object.freeze({ ConsoleStorage, gameSaved });
}
