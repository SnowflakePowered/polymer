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

function statement(sql, open) {
  sql = String(sql);
  return Object.freeze({
    run(values) { open(); return op_database_run(sql, parameters(values)); },
    async get(values) { open(); return (await op_database_all(sql, parameters(values)))[0]; },
    all(values) { open(); return op_database_all(sql, parameters(values)); },
    async *iterate(values) { open(); for (const row of await op_database_all(sql, parameters(values))) yield row; },
  });
}

function database() {
  let closed = false;
  const open = () => {
    if (closed) throw new Error("plugin database is closed");
  };
  const value = {
    prepare(sql) { open(); return statement(sql, open); },
    exec(sql) { open(); return op_database_exec(String(sql)); },
    async batch(items, mode = "write") {
      open();
      const begin = mode === "read" ? "BEGIN" : "BEGIN IMMEDIATE";
      await op_database_exec(begin);
      try {
        const results = [];
        for (const item of items) {
          const [sql, params = []] = Array.isArray(item) ? item : [item.sql, item.args ?? []];
          const text = String(sql);
          results.push(/^\s*(?:SELECT|PRAGMA|WITH)\b/i.test(text)
            ? await op_database_all(text, parameters(params))
            : await op_database_run(text, parameters(params)));
        }
        await op_database_exec("COMMIT");
        return results;
      } catch (error) { await op_database_exec("ROLLBACK"); throw error; }
    },
    async transactionAsync(callback, mode = "write") {
      open();
      await op_database_exec(mode === "read" ? "BEGIN" : "BEGIN IMMEDIATE");
      try { const result = await callback(value); await op_database_exec("COMMIT"); return result; }
      catch (error) { await op_database_exec("ROLLBACK"); throw error; }
    },
    close() {
      if (!closed) op_database_close();
      closed = true;
      return Promise.resolve();
    },
  };
  return Object.freeze(value);
}

Object.defineProperty(globalThis, "__snowglassDatabaseConnect", {
  value: async () => {
    await op_database_connect();
    return database();
  },
});
