import type { PluginDatabase } from "polymer:api";

/** Bounded multi-row inserts; the XML reader awaits flush before reading on. */
export class IndexWriter {
  private rows: unknown[][] = [];
  private written = 0;
  private reported = 0;

  constructor(private db: PluginDatabase, private insert: string, private label: string) {}

  add(row: unknown[]) {
    this.rows.push(row);
  }

  async flush() {
    while (this.rows.length) {
      // Nine columns at most: stay below SQLite's conservative parameter limit.
      const batch = this.rows.splice(0, 100);
      const placeholders = batch.map(row => `(${row.map(() => "?").join(",")})`).join(",");
      await this.db.prepare(`${this.insert} VALUES ${placeholders}`).run(batch.flat());
      this.written += batch.length;
    }
    if (this.written - this.reported >= 100_000) {
      console.info(`LaunchBox: indexed ${this.written} ${this.label}`);
      this.reported = this.written;
    }
  }
}
