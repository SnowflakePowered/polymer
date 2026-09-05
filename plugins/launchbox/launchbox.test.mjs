// Run: node --experimental-vm-modules --test plugins/launchbox/launchbox.test.mjs
import assert from "node:assert/strict";
import { readFile } from "node:fs/promises";
import { stripTypeScriptTypes } from "node:module";
import { DatabaseSync } from "node:sqlite";
import { SourceTextModule, SyntheticModule, createContext } from "node:vm";
import { test } from "node:test";

class ImportFixture {
  sql = new DatabaseSync(":memory:");
  calls = 0;
  progress = [];
  failExtraction = false;

  async load() {
    const fixture = this;
    const db = {
      exec: async sql => this.sql.exec(sql),
      prepare: sql => ({
        run: async args => { this.calls++; return this.sql.prepare(sql).run(...args); },
        get: async (args = []) => this.sql.prepare(sql).get(...args),
        all: async (args = []) => this.sql.prepare(sql).all(...args),
      }),
    };
    class ZipReader {
      async getEntries() {
        return ["Metadata.xml", "Files.xml"].map((filename, kind) => ({
          filename, uncompressedSize: 4,
          async getData(stream) {
            const writer = stream.getWriter();
            if (fixture.failExtraction) { await writer.abort(new Error("Bad archive")); throw new Error("Bad archive"); }
            await writer.write(new Uint8Array([kind, 0]));
            await writer.write(new Uint8Array([kind, 1]));
            await writer.close();
          },
        }));
      }
      async close() {}
    }
    const parser = async (chunks, events) => {
      const record = (name, fields) => {
        events.onStartElement(name);
        for (const [key, value] of Object.entries(fields)) {
          events.onStartElement(key); events.onText(String(value)); events.onEndElement(key);
        }
        events.onEndElement(name);
      };
      for await (const [kind, batch] of chunks) {
        if (batch === 1) {
          const table = kind === 0 ? "games_next" : "raw_aliases_next";
          assert.equal(fixture.sql.prepare(`SELECT COUNT(*) n FROM ${table}`).get().n, 250, "writes must finish before parsing the next chunk");
        }
        for (let i = batch * 250 + 1; i <= (batch + 1) * 250; i++) {
          if (kind === 0) {
            record("Game", { DatabaseID: i, Platform: "Nintendo GameCube", Name: `Game ${i}`, Overview: "A test game" });
            record("GameImage", { DatabaseID: i, FileName: `${i}.png`, Type: "Box - Front", Region: "Japan" });
            if (i === 42) {
              record("GameImage", { DatabaseID: i, FileName: "42-us.png", Type: "Box - Front", Region: "North America" });
              record("GameImage", { DatabaseID: i, FileName: "42-eu.png", Type: "Box - Front", Region: "Europe" });
              record("GameImage", { DatabaseID: i, FileName: "42-neutral.png", Type: "Banner" });
            }
          } else record("File", { Platform: "Nintendo GameCube", FileName: `Alias ${i} (USA)`, GameName: `Game ${i}` });
        }
      }
    };
    const context = createContext({ console, TransformStream, Uint8Array, fetch: async () => new Response(new Uint8Array([1])) });
    const synthetic = exports => new SyntheticModule(Object.keys(exports), function () {
      for (const [key, value] of Object.entries(exports)) this.setExport(key, value);
    }, { context });
    const imports = {
      "polymer:database": synthetic({ connect: async () => db }),
      "polymer:cache": synthetic({ caches: { download: { open: () => ({ size: 4, writable: new WritableStream() }) } } }),
      "jsr:@std/xml/parse-stream": synthetic({ parseXmlStreamFromBytes: parser }),
      "jsr:@zip-js/zip-js": synthetic({ Reader: class {}, ZipReader }),
    };
    const writer = new SourceTextModule(stripTypeScriptTypes(await readFile(new URL("./index-writer.ts", import.meta.url), "utf8"), { mode: "transform" }), { context });
    imports["pkg:index-writer.ts"] = writer;
    const plugin = new SourceTextModule(stripTypeScriptTypes(await readFile(new URL("./launchbox.ts", import.meta.url), "utf8"), { mode: "transform" }), { context });
    await plugin.link(specifier => imports[specifier]);
    await plugin.evaluate();
    return plugin.namespace;
  }
}

test("onPluginLoad batches and finishes indexing before scraping aliases", async () => {
  const fixture = new ImportFixture();
  const plugin = await fixture.load();
  await plugin.onPluginLoad({ progress: value => fixture.progress.push(value) });
  assert.equal(fixture.sql.prepare("SELECT value FROM content_state WHERE key = 'ready'").get().value, "2");
  assert.equal(fixture.calls, 18, "1500 rows should use 18 batched calls, not 1500 individual calls");
  const values = [];
  for await (const value of plugin.onScrape({
    "package.platform": [{ value: "NINTENDO_GCN", source: "test" }],
    "filename.nointro.title": [{ value: "Alias 42", source: "test" }],
  })) values.push(value);
  assert.ok(values.some(value => value.key === "meta.title" && value.value === "Game 42"));
  assert.ok(values.some(value => value.key === "media.boxart.front" && value.value.endsWith("42.png")));
  assert.ok(fixture.progress.some(value => value.message === "Indexing filenames" && value.completed === value.total));
  const calls = fixture.calls;
  await plugin.onPluginLoad({ progress() {} });
  assert.equal(fixture.calls, calls, "ready indexes must not be rebuilt");
  fixture.sql.close();
});

test("failed archive extraction rejects plugin loading instead of hanging", async () => {
  const fixture = new ImportFixture();
  fixture.failExtraction = true;
  const plugin = await fixture.load();
  await assert.rejects(plugin.onPluginLoad({ progress() {} }), /Bad archive/);
  assert.equal(fixture.sql.prepare("SELECT COUNT(*) n FROM games_next").get().n, 0);
  fixture.sql.close();
});


test("USA filename region selects North American covers with neutral banner fallback after index migration", async () => {
  const fixture = new ImportFixture();
  const plugin = await fixture.load();
  await plugin.onPluginLoad({ progress() {} });
  fixture.sql.exec("UPDATE content_state SET value = '1' WHERE key = 'ready'; ALTER TABLE images DROP COLUMN region;");
  await plugin.onPluginLoad({ progress() {} });
  assert.equal(fixture.sql.prepare("SELECT region FROM images WHERE url LIKE '%42-us.png'").get().region, "North America");
  const scrape = async region => {
    const values = [];
    for await (const value of plugin.onScrape({
      "package.platform": [{ value: "NINTENDO_GCN", source: "test" }],
      "filename.nointro.title": [{ value: "Alias 42", source: "host:shiratsu" }],
      "filename.nointro.region": [{ value: region, source: "host:shiratsu" }],
    })) if (value.key.startsWith("media.")) values.push(value.value.split("/").pop());
    return values;
  };
  assert.deepEqual(await scrape("USA"), ["42-neutral.png", "42-us.png"]);
  assert.deepEqual(await scrape("Europe"), ["42-neutral.png", "42-eu.png"]);
  assert.deepEqual(await scrape("Australia"), ["42-neutral.png"]);
  assert.deepEqual(await scrape("USA, Europe"), ["42-neutral.png", "42-us.png", "42-eu.png"]);
  fixture.sql.close();
});


test("updateContents forwards progress through downloading and indexing", async () => {
  const fixture = new ImportFixture();
  const plugin = await fixture.load();
  await plugin.onPluginLoad({ progress() {} });
  // The first refresh can reuse the index just built by onPluginLoad.
  await plugin.updateContents({ progress() {} });
  const updates = [];
  await plugin.updateContents({ progress: value => updates.push(value) });
  assert.ok(updates.some(value => value.message === "Downloading LaunchBox metadata"));
  assert.ok(updates.some(value => value.message === "Indexing game metadata" && value.completed === value.total));
  assert.ok(updates.some(value => value.message === "Indexing filenames" && value.completed === value.total));
  fixture.sql.close();
});
