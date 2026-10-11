import type { LaunchRequest, ScrapingApi } from "./polymer";

export type StartupRequest =
  | { kind: "scrape"; path: string }
  | { kind: "package"; path: string; emulator?: string }
  | { kind: "launch"; options: LaunchRequest };

/** Buffers host requests to scrape, open or launch until a shell accepts them. */
export class StartupRequests {
  private queue: StartupRequest[] = [];
  private handler: ((request: StartupRequest) => void) | null = null;

  constructor(private scraping: ScrapingApi) {
    scraping.addEventListener("request", this.received);
  }

  accept(handler: (request: StartupRequest) => void) {
    this.handler = handler;
    for (const request of this.queue.splice(0)) handler(request);
    return () => { if (this.handler === handler) this.handler = null; };
  }

  dispose() {
    this.scraping.removeEventListener("request", this.received);
  }

  private readonly received = (event: Event) => {
    const request = (event as CustomEvent<StartupRequest>).detail;
    if (this.handler) this.handler(request);
    else this.queue.push(request);
  };
}
