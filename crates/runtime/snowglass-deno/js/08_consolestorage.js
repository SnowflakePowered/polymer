import {
  op_consolestorage_create,
  op_consolestorage_delete,
  op_consolestorage_list,
  op_consolestorage_set_label,
} from "ext:core/ops";

export function createConsoleStorage() {
  const nandData = new WeakMap();
  const internal = Symbol("Snowglass.consolestorage.Nand");

  class Nand {
    constructor(token, data) {
      if (token !== internal) throw new TypeError("Use Snowglass.consolestorage.Nand.create()");
      nandData.set(this, Object.freeze({ ...data }));
      Object.freeze(this);
    }

    static async create(platform, options = undefined) {
      if (typeof platform !== "string" || platform.length === 0) {
        throw new TypeError("A nand requires a platform id");
      }
      const label = options?.label === undefined ? null : String(options.label);
      return new Nand(internal, await op_consolestorage_create(platform, label));
    }

    static async list() {
      const nands = await op_consolestorage_list();
      return nands.map((data) => new Nand(internal, data));
    }

    get id() { return nandData.get(this).id; }
    get platform() { return nandData.get(this).platform; }
    get label() { return nandData.get(this).label; }
    get created() { return new Date(nandData.get(this).created); }
    get path() { return nandData.get(this).path; }

    async setLabel(label) {
      const data = nandData.get(this);
      const next = label === null || label === undefined ? null : String(label);
      return new Nand(internal, await op_consolestorage_set_label(data.platform, data.id, next));
    }

    async delete() {
      const data = nandData.get(this);
      await op_consolestorage_delete(data.platform, data.id);
    }

    toJSON() { return { ...nandData.get(this) }; }
  }

  const ConsoleStorage = Object.freeze({ Nand });
  return Object.freeze({ ConsoleStorage });
}
