import { op_keys_list, op_keys_open, op_keys_put, op_keys_remove } from "ext:core/ops";

const identity = value => {
  if (value === null || typeof value !== "object" || !["system", "content"].includes(value.namespace) || typeof value.id !== "string" || !value.id)
    throw new TypeError("key identity requires a namespace and nonempty id");
  return { namespace: value.namespace, id: value.id };
};

const reader = Object.freeze({
  async list(query = {}) {
    if (query === null || typeof query !== "object") throw new TypeError("key query must be an object");
    const filter = {};
    if (query.namespace !== undefined) {
      if (!["system", "content"].includes(query.namespace)) throw new TypeError("key namespace must be system or content");
      filter.namespace = query.namespace;
    }
    if (query.titleId !== undefined) filter.titleId = query.titleId;
    return Object.freeze((await op_keys_list(filter))
      .map(item => Object.freeze(item)));
  },
  async open(value) {
    return __polymerFilesystem.fileResource(await op_keys_open(identity(value)));
  },
});

const writer = Object.freeze({
  ...reader,
  async put(records) {
    if (!Array.isArray(records)) throw new TypeError("key records must be an array");
    await op_keys_put(records.map(record => {
      const key = identity(record);
      if (!(record.bytes instanceof Uint8Array)) throw new TypeError("key bytes must be Uint8Array");
      if (record.titleId !== undefined && typeof record.titleId !== "string") throw new TypeError("titleId must be a string");
      return { ...key, ...(record.titleId === undefined ? {} : { titleId: record.titleId }), bytes: Array.from(record.bytes) };
    }));
  },
  async remove(value) { return op_keys_remove(identity(value)); },
});

Object.defineProperty(globalThis, "__polymerKeys", { value: Object.freeze({ reader, writer }) });
