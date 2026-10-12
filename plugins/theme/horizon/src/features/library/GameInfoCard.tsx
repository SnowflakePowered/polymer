import { Show, createResource, createSignal, onCleanup, onMount } from "solid-js";
import type { RecentGame } from "../../app/models";
import { useShell } from "../../app/shell-context";
import { ActionHints, CoverArt, DialogParts as Dialog, OverlayPortal, type ActionHint } from "../../ui";
import "./GameInfoCard.css";

export function GameInfoCard(props: { game: RecentGame; returnFocus(): HTMLElement | null | undefined; onClose(): void }) {
  const shell = useShell();
  let frame!: HTMLDivElement, text!: HTMLDivElement, probe!: HTMLSpanElement;
  const [page, setPage] = createSignal(0);
  const [layout, setLayout] = createSignal({ height: 0, pages: 1 });
  const [info] = createResource(() => props.game.path, async path => {
    const launchable = await window.Polymer.library.launchables.find(path);
    const game = launchable && await window.Polymer.library.games.get(launchable.parentBinaryId ?? launchable.binaryId);
    const values = (key: string) => (game?.metadata ?? [])
      .filter(item => item.key === key && item.data.kind === "scalar")
      .sort((a, b) => a.ordinal - b.ordinal)
      .map(item => String(item.data.kind === "scalar" ? item.data.value : ""));
    return {
      title: values("meta.title")[0], description: values("meta.description")[0], publisher: values("meta.publisher")[0],
      released: values("meta.releasedate")[0]?.slice(0, 4), genres: values("meta.genre"), rating: values("meta.rating.esrb")[0],
    };
  });
  const current = () => Math.min(page(), layout().pages - 1);
  const turn = (step: number) => setPage(Math.max(0, Math.min(layout().pages - 1, current() + step)));
  const measure = () => {
    const line = probe.getBoundingClientRect().height || 1;
    const height = Math.max(line, Math.floor(frame.getBoundingClientRect().height / line) * line);
    setLayout({ height, pages: Math.max(1, Math.ceil(text.getBoundingClientRect().height / height - .01)) });
  };
  onMount(() => {
    const observer = new ResizeObserver(measure);
    observer.observe(frame);
    observer.observe(text);
    onCleanup(() => observer.disconnect());
  });
  const hints = (): ActionHint[] => [
    ...(layout().pages > 1 ? [{ action: current() < layout().pages - 1 ? "next" : "previous", label: current() < layout().pages - 1 ? "Next" : "Previous" } as const] : []),
    { action: "details", label: "Settings" }, { action: "activate", label: "Start" }, { action: "back", label: "Back" },
  ];
  const handle = (action: { type: string; direction?: string }) => {
    if (action.type === "next" || (action.type === "move" && action.direction === "down")) turn(1);
    else if (action.type === "previous" || (action.type === "move" && action.direction === "up")) turn(-1);
    else if (action.type === "activate") { const game = props.game; props.onClose(); shell.launchRecent(game); }
    else if (action.type === "details") { const game = props.game; props.onClose(); shell.showDetails(game); }
    else return action.type === "move";
    return true;
  };
  const platform = () => window.Polymer.machine(props.game.platform)?.name ?? props.game.platform;

  return <Dialog.Root open initialFocusEl={() => frame} onOpenChange={details => { if (!details.open) props.onClose(); }}>
    <OverlayPortal>
      <Dialog.Backdrop class="horizon-info-backdrop" />
      <Dialog.Positioner class="horizon-info-shell">
        <Dialog.Content class="horizon-info-card" returnFocus={props.returnFocus} hints={hints} onAction={handle}>
          <aside class="horizon-info-art">
            <CoverArt src={props.game.artwork} blurhash={props.game.artworkBlurhash} alt={props.game.name} />
          </aside>
          <section class="horizon-info-body">
            <Dialog.Title class="horizon-info-title">{info()?.title ?? props.game.name}</Dialog.Title>
            <div class="horizon-info-meta">
              <p><Show when={info()?.publisher}>{publisher => <span>{publisher()}</span>}</Show><span>{platform()}</span><Show when={info()?.released}>{year => <span>Released in: {year()}</span>}</Show></p>
              <Show when={info()?.genres.length}><p><span>Genre: {info()!.genres.join(", ")}</span><Show when={info()?.rating}>{rating => <span>Rating: {rating()}</span>}</Show></p></Show>
            </div>
            <div ref={frame} class="horizon-info-frame" tabIndex={-1} data-input-default>
              <span ref={probe} class="horizon-info-probe" aria-hidden="true">M</span>
              <div class="horizon-info-window" style={{ height: `${layout().height}px` }}>
                <div ref={text} class="horizon-info-description" data-empty={!info()?.description || undefined} style={{ transform: `translateY(${-current() * layout().height}px)` }}>
                  {info.loading ? "" : info()?.description ?? "No description available."}
                </div>
              </div>
            </div>
            <p class="horizon-info-page" aria-live="polite">{layout().pages > 1 ? `${current() + 1}/${layout().pages}` : ""}</p>
          </section>
          <ActionHints floating hints={hints()} />
        </Dialog.Content>
      </Dialog.Positioner>
    </OverlayPortal>
  </Dialog.Root>;
}
