import { caches } from "polymer:cache";
import { connect } from "polymer:database";
import { Job } from "polymer:jobs";
import type { OnPluginLoad, OnScrape, UpdateContents, ScrapeInput, ScrapeResult } from "polymer:api";
import { ShiragameContent } from "pkg:content.ts";
import { ShiragameMatcher } from "pkg:matching.ts";

class ShiragameScraper {
  readonly content = new ShiragameContent();

  load(): Promise<boolean> {
    return this.content.ready();
  }

  async update(): Promise<void> {
    await new Job("Updating Shiragame database").run(job => this.content.update(job));
  }

  async *scrape(input: ScrapeInput): AsyncGenerator<ScrapeResult> {
    const generation = await this.content.current();
    if (!generation) throw new Error("Shiragame database is not initialized; refresh plugin contents");
    const db = await connect(caches.download.open(generation.path, { read: false }));
    try {
      yield* new ShiragameMatcher(db, generation.release).scrape(input);
    } finally { await db.close(); }
  }
}

const scraper = new ShiragameScraper();
export const onPluginLoad: OnPluginLoad = scraper.load.bind(scraper);
export const updateContents: UpdateContents = scraper.update.bind(scraper);
export const onScrape: OnScrape = scraper.scrape.bind(scraper);
export default { onPluginLoad, updateContents, onScrape };
