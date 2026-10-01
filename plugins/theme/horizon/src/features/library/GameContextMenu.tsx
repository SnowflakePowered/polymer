import type { RecentGame } from "../../app/models";
import { useShell } from "../../app/shell-context";
import { ActionDialog } from "../../ui";

export function GameContextMenu(props: { game: RecentGame; onClose(): void }) {
  const shell = useShell();
  const find = async () => (await window.Polymer.library.games.list())
    .find(game => game.launchables.some(item => item.uri === props.game.path));
  const rescrape = async () => {
    try {
      const game = await find();
      if (game) await window.Polymer.scraping.start(game.id);
    } catch (error) {
      shell.setModel("error", `Could not rescrape ${props.game.name}: ${String(error)}`);
    }
  };
  const remove = async () => {
    const selected = props.game;
    try {
      const game = await find();
      if (game) await window.Polymer.library.games.delete(game.id);
      const paths = new Set(game?.launchables.map(item => item.uri) ?? [selected.path]);
      shell.setModel("recent", (entries: RecentGame[]) => entries.filter(entry => !paths.has(entry.path)));
      shell.setModel("libraryGames", (entries: RecentGame[]) => entries.filter(entry => !paths.has(entry.path)));
    } catch (error) {
      shell.setModel("error", `Could not delete ${selected.name}: ${String(error)}`);
    }
  };
  return <ActionDialog open title={props.game.name} description={window.Polymer.machine(props.game.platform)?.name ?? props.game.platform} onClose={props.onClose}
    actions={[{ label: "Rescrape", run: () => void rescrape() }, { label: "Delete game", danger: true, run: () => void remove() }]} />;
}
