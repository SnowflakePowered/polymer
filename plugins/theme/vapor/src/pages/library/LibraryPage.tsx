import { For, Show, createSignal } from "solid-js";
import { useShell, type LibrarySort } from "../../app/shell";
import { Capsule, CapsuleGrid, ContextMenu, EmptyState, Page, Tabs } from "../../ui";
import { platformName } from "./platform";
import "./library.css";

const SORTS: Record<LibrarySort, string> = { title: "Alphabetical", platform: "By platform", added: "Recently added" };

export function LibraryPage() {
  const shell = useShell();
  const [sortOpen, setSortOpen] = createSignal(false);
  const tabs = () => [{ id: "", title: "All games" }, ...shell.state.platforms.map(platform => ({ id: platform, title: window.Polymer.machine(platform)?.shortName ?? platformName(platform) }))];
  const loadMore = (event: Event) => {
    const element = event.currentTarget as HTMLElement;
    if (element.scrollTop + element.clientHeight > element.scrollHeight - 600) shell.library.loadMore();
  };
  return <Page class="vp-library" focusWhen={() => shell.state.games.length} hints={[{ action: "activate", label: "Select" }, { action: "details", label: "Sort" }, { action: "back", label: "Back" }]} onAction={action => {
    if (action.type === "details") { setSortOpen(true); return true; }
    if (action.type === "back") { shell.update("menu", "main"); return true; }
    return false;
  }}>
    <header class="vp-library-header">
      <Tabs tabs={tabs()} active={shell.state.query.platform ?? ""} onChange={id => shell.query({ platform: id || null })} />
      <div class="vp-library-tools">
        <input class="vp-text-input vp-library-search" type="search" placeholder="Search" aria-label="Search library" value={shell.state.query.text} onInput={event => shell.query({ text: event.currentTarget.value })} />
        <button type="button" class="vp-library-sort" onClick={() => setSortOpen(true)}><span>{SORTS[shell.state.query.sort]}</span></button>
      </div>
    </header>
    <div class="vp-library-scroll" onScroll={loadMore}>
      <div class="vp-library-count">{shell.state.total} games</div>
      <Show when={shell.state.games.length} fallback={<EmptyState title={shell.state.filtered ? "No games match" : "Your library is empty"} />}>
        <CapsuleGrid><For each={shell.state.games}>{(game, index) =>
          <Capsule title={game.name} subtitle={platformName(game.platform)} art={game.artwork} ratio={game.artworkRatio} data-autofocus={index() === 0} badge={game.missingKey ? "Missing key" : undefined}
            progress={game.scraping ? game.scraping.completed / Math.max(1, game.scraping.total) : undefined} onClick={() => void shell.open(game, "details")} />
        }</For></CapsuleGrid>
      </Show>
    </div>
    <ContextMenu open={sortOpen()} onClose={() => setSortOpen(false)} title="Sort by"
      actions={(Object.keys(SORTS) as LibrarySort[]).map(sort => ({ label: SORTS[sort], run: () => shell.query({ sort }) }))} />
  </Page>;
}
