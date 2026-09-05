import assert from "node:assert/strict";
import { test } from "node:test";
import { LibraryState } from "./library-state.ts";

const flush = () => new Promise(resolve => setImmediate(resolve));

class Fixture {
  library = new EventTarget();
  scraping = new EventTarget();
  snapshots = [];
  errors = [];
  games = [];
  job = { id: "job", rootBinaryId: "root", triggerBinaryId: "root", state: "queued", createdAt: 1, updatedAt: 1, attempts: [] };
  constructor() {
    this.library.games = { list: async () => this.games };
    this.scraping.jobs = { list: async () => [], start: async () => this.job };
    this.state = new LibraryState(this.library, this.scraping, snapshot => this.snapshots.push(snapshot), error => this.errors.push(error));
  }
  get latest() { return this.snapshots.at(-1); }
  notify(job) { this.scraping.dispatchEvent(new CustomEvent("jobstatechange", { detail: job })); }
}

test("catalog loads unplayed games and content events refresh scraped titles", async () => {
  const fixture = new Fixture();
  fixture.games = [{ id: "root", platform: "NINTENDO_NES", launchables: [{ path: "C:\\games\\game.nes", parentBinaryId: null }], metadata: [] }];
  fixture.state.start();
  await flush();
  assert.equal(fixture.latest.games[0].name, "game.nes");
  fixture.games[0].metadata = [{ key: "meta.title.en", data: { kind: "scalar", value: "Scraped title" } }];
  fixture.library.dispatchEvent(new CustomEvent("contentchange"));
  await flush();
  assert.equal(fixture.latest.games[0].name, "Scraped title");
  assert.equal(fixture.latest.games[0].playedAt, 0);
  fixture.state.dispose();
  const count = fixture.snapshots.length;
  fixture.library.dispatchEvent(new CustomEvent("contentchange"));
  await flush();
  assert.equal(fixture.snapshots.length, count);
});

test("completion notification wins over late enqueue and startup responses", async () => {
  const fixture = new Fixture();
  let resolveJobs;
  fixture.scraping.jobs.list = () => new Promise(resolve => { resolveJobs = resolve; });
  let resolveImport;
  fixture.library.launchables = { add: () => new Promise(resolve => { resolveImport = resolve; }) };
  fixture.state.start();
  const pending = fixture.state.import("game.nes");
  assert.match(fixture.latest.status, /Identifying game.nes/);
  fixture.notify({ ...fixture.job, state: "completed", updatedAt: 2, attempts: [{ pluginId: "fixture", state: "completed", error: null }] });
  resolveImport({ launchable: {}, scrapeJob: fixture.job });
  resolveJobs([fixture.job]);
  await pending;
  await flush();
  assert.match(fixture.latest.status, /completed \(1\/1 scrapers\)/);
  assert.doesNotMatch(fixture.latest.status, /queued|Identifying/);
  fixture.state.dispose();
});

test("progress names active scrapers and exposes partial failure details", async () => {
  const fixture = new Fixture();
  fixture.state.start();
  fixture.notify({ ...fixture.job, state: "running", attempts: [{ pluginId: "database", state: "running", error: null }] });
  assert.match(fixture.latest.status, /running \(0\/1 scrapers\).*database/);
  fixture.notify({ ...fixture.job, state: "partial", attempts: [{ pluginId: "database", state: "failed", error: "Download failed" }] });
  assert.match(fixture.latest.status, /partial \(1\/1 scrapers\).*database: Download failed/);
  await flush();
  fixture.state.dispose();
});

test("polling recovers missed completion and content notifications and stops on disposal", async (t) => {
  t.mock.timers.enable({ apis: ["setTimeout"] });
  const fixture = new Fixture();
  let polls = 0;
  fixture.scraping.jobs.list = async () => { polls++; return [fixture.job]; };
  fixture.state.start();
  await flush();
  assert.match(fixture.latest.status, /queued/);
  fixture.games = [{ id: "root", platform: "NINTENDO_NES", launchables: [{ path: "game.nes" }], metadata: [] }];
  fixture.job = { ...fixture.job, state: "completed", updatedAt: 2, attempts: [{ pluginId: "database", state: "completed", error: null }] };
  t.mock.timers.tick(1000);
  await flush();
  assert.match(fixture.latest.status, /completed/);
  assert.equal(fixture.latest.games[0].name, "game.nes");
  assert.equal(polls, 2);
  fixture.state.dispose();
  t.mock.timers.tick(5000);
  await flush();
  assert.equal(polls, 2);
});

test("polling retries failures without overlapping pending requests", async (t) => {
  t.mock.timers.enable({ apis: ["setTimeout"] });
  const fixture = new Fixture();
  let polls = 0;
  let resolvePoll;
  fixture.scraping.jobs.list = async () => {
    polls++;
    if (polls === 1) throw new Error("Temporary failure");
    return new Promise(resolve => { resolvePoll = resolve; });
  };
  fixture.state.start();
  await flush();
  assert.equal(fixture.errors.length, 1);
  t.mock.timers.tick(1000);
  await flush();
  t.mock.timers.tick(5000);
  assert.equal(polls, 2);
  fixture.state.dispose();
  const snapshots = fixture.snapshots.length;
  resolvePoll([fixture.job]);
  await flush();
  assert.equal(fixture.snapshots.length, snapshots);
});
