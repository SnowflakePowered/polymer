import {
  op_consolestorage_create,
  op_consolestorage_delete,
  op_consolestorage_game_saved,
  op_consolestorage_list,
  op_consolestorage_set_label,
} from "ext:core/ops";

export function createConsoleStorage() {
  const storageData = new WeakMap();
  const watching = new WeakSet();
  const deleted = new WeakSet();
  const internal = Symbol("Snowglass.consolestorage.Storage");

  async function receiveGameSaved(storage) {
    const data = storageData.get(storage);
    while (!deleted.has(storage)) {
      try {
        await op_consolestorage_game_saved(data.kind, data.platform, data.id);
      } catch (error) {
        if (!deleted.has(storage)) {
          storage.dispatchEvent(new CustomEvent("error", { detail: error }));
        }
        return;
      }
      if (!deleted.has(storage)) storage.dispatchEvent(new Event("gamesaved"));
    }
  }

  class Storage extends EventTarget {
    constructor(token, data) {
      super();
      if (token !== internal) throw new TypeError("Use Snowglass.consolestorage.Storage.create()");
      storageData.set(this, Object.freeze({ ...data }));
    }

    addEventListener(type, callback, options) {
      super.addEventListener(type, callback, options);
      if (String(type) === "gamesaved" && !watching.has(this)) {
        watching.add(this);
        void receiveGameSaved(this);
      }
    }

    static async create(kind, platform, options = undefined) {
      if (!["nand", "saveRepo", "memoryCard"].includes(kind)) {
        throw new TypeError("Unknown console storage kind");
      }
      if (typeof platform !== "string" || platform.length === 0) {
        throw new TypeError("Console storage requires a platform id");
      }
      const label = options?.label === undefined ? null : String(options.label);
      return new Storage(internal, await op_consolestorage_create(kind, platform, label));
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
      await op_consolestorage_delete(data.kind, data.platform, data.id);
    }

    toJSON() { return { ...storageData.get(this) }; }
  }

  const ConsoleStorage = Object.freeze({ Storage });
  return Object.freeze({ ConsoleStorage });
}
