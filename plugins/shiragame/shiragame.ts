import { caches } from "polymer:cache";
import { connect } from "polymer:database";
import type { OnPluginLoad, OnScrape, UpdateContents, PluginProgressContext, ScrapeInput, ScrapeResult } from "polymer:api";
import { ShiragameContent } from "pkg:content.ts";
import { ShiragameMatcher } from "pkg:matching.ts";

class ShiragameScraper {
  readonly content = new ShiragameContent();
  private initializedFresh = false;

  async load(context: PluginProgressContext): Promise<void> {
    this.initializedFresh = await this.content.initialize(context);
  }

  async update(context: PluginProgressContext): Promise<void> {
    // The host runs onPluginLoad before the first explicit update as well. Its
    // successful first download already satisfies that update in this isolate.
    if (this.initializedFresh) { this.initializedFresh = false; return; }
    await this.content.refresh(context);
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
