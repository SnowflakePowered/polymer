import { For, Show, createSignal } from "solid-js";
import { useShell, type LibrarySort } from "../../app/shell";
import { Capsule, CapsuleGrid, ContextMenu, EmptyState, Icon, Page, Tabs } from "../../ui";
import { platformName } from "./platform";
import { AddGameModal } from "./AddGameModal";
import "./library.css";

const SORTS: Record<LibrarySort, string> = { title: "Alphabetical", platform: "By platform", added: "Recently added" };

export function LibraryPage() {
  const shell = useShell();
  const [sortOpen, setSortOpen] = createSignal(false);
  const [adding, setAdding] = createSignal(false);
  let sort!: HTMLButtonElement;
  const tabs = () => [{ id: "", title: "All games" }, ...shell.state.platforms.map(platform => ({ id: platform, title: platformName(platform) }))];
  const loadMore = (event: Event) => {
    const element = event.currentTarget as HTMLElement;
    if (element.scrollTop + element.clientHeight > element.scrollHeight - 600) shell.library.loadMore();
  };
  return <Page class="mc-library" focusWhen={() => shell.state.games.length} hints={[{ action: "activate", label: "Select" }, { action: "details", label: "Sort" }, { action: "back", label: "Back" }]} onAction={action => {
    if (action.type === "details") { setSortOpen(true); return true; }
    if (action.type === "back") { shell.update("menu", "main"); return true; }
    return false;
  }}>
    <header class="mc-library-header">
      <Tabs tabs={tabs()} active={shell.state.query.platform ?? ""} onChange={id => shell.query({ platform: id || null })} />
      <div class="mc-library-tools">
        <button ref={sort} type="button" class="mc-library-sort" onClick={() => setSortOpen(true)}><Icon name="sort" size={16} /><span>{SORTS[shell.state.query.sort]}</span></button>
        <div class="mc-library-spacer" />
        <span class="mc-library-count">{shell.state.total} {shell.state.total === 1 ? "game" : "games"}</span>
        <button type="button" class="mc-library-add" aria-label="Add a game to your library" title="Add a game to your library" onClick={() => setAdding(true)}><Icon name="add" size={20} /></button>
      </div>
    </header>
    <div class="mc-library-scroll" onScroll={loadMore}>
      <Show when={shell.state.games.length} fallback={<EmptyState title={shell.state.filtered ? "No games match" : "Your library is empty"} />}>
        <CapsuleGrid><For each={shell.state.games}>{(game, index) =>
          <Capsule busy={shell.state.closing === game.path} title={game.name} subtitle={platformName(game.platform)} art={game.artwork} data-autofocus={index() === 0} badge={game.missingKey ? "Missing key" : undefined}
            progress={game.scraping ? game.scraping.completed / Math.max(1, game.scraping.total) : undefined} onClick={() => void shell.open(game, "details")} />
        }</For></CapsuleGrid>
      </Show>
    </div>
    <AddGameModal open={adding()} onClose={() => setAdding(false)} />
    <ContextMenu open={sortOpen()} onClose={() => setSortOpen(false)} title="Sort by" anchor={() => sort}
      actions={(Object.keys(SORTS) as LibrarySort[]).map(sort => ({ label: SORTS[sort], run: () => shell.query({ sort }) }))} />
  </Page>;
}
