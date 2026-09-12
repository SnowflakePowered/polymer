import { cacheFileLocation } from "ext:polymer_cache/00_cache.js";
import {
  op_database_all,
  op_database_close,
  op_database_connect,
  op_database_exec,
  op_database_run,
} from "ext:core/ops";

function parameters(value) {
  if (value === undefined) return [];
  return Array.isArray(value) ? value : [value];
}

function statement(id, sql, open) {
  sql = String(sql);
  return Object.freeze({
    run(values) { open(); return op_database_run(id, sql, parameters(values)); },
    async get(values) { open(); return (await op_database_all(id, sql, parameters(values)))[0]; },
    all(values) { open(); return op_database_all(id, sql, parameters(values)); },
    async *iterate(values) { open(); for (const row of await op_database_all(id, sql, parameters(values))) yield row; },
  });
}

function database(id) {
  let closed = false;
  const open = () => {
    if (closed) throw new Error("plugin database is closed");
  };
  const value = {
    prepare(sql) { open(); return statement(id, sql, open); },
    exec(sql) { open(); return op_database_exec(id, String(sql)); },
    async batch(items, mode = "write") {
      open();
      const begin = mode === "read" ? "BEGIN" : "BEGIN IMMEDIATE";
      await op_database_exec(id, begin);
      try {
        const results = [];
        for (const item of items) {
          const [sql, params = []] = Array.isArray(item) ? item : [item.sql, item.args ?? []];
          const text = String(sql);
          results.push(/^\s*(?:SELECT|PRAGMA|WITH)\b/i.test(text)
            ? await op_database_all(id, text, parameters(params))
            : await op_database_run(id, text, parameters(params)));
        }
        await op_database_exec(id, "COMMIT");
        return results;
      } catch (error) { await op_database_exec(id, "ROLLBACK"); throw error; }
    },
    async transactionAsync(callback, mode = "write") {
      open();
      await op_database_exec(id, mode === "read" ? "BEGIN" : "BEGIN IMMEDIATE");
      try { const result = await callback(value); await op_database_exec(id, "COMMIT"); return result; }
      catch (error) { await op_database_exec(id, "ROLLBACK"); throw error; }
    },
    close() {
      if (!closed) op_database_close(id);
      closed = true;
      return Promise.resolve();
    },
  };
  return Object.freeze(value);
}

Object.defineProperty(globalThis, "__polymerDatabaseConnect", {
  value: async (file, options = {}) => {
    const location = file === undefined ? null : cacheFileLocation(file);
    const readOnly = options.readOnly ?? (file !== undefined);
    if (typeof readOnly !== "boolean") throw new TypeError("readOnly must be a boolean");
    return database(await op_database_connect(location, readOnly));
  },
});
