import { friendly } from "../../app/launch";
import type { RecentGame } from "../../app/models";
import { useShell } from "../../app/shell-context";
import { ContextMenu } from "../../components/ContextMenu";

export function GameContextMenu(props: { game: RecentGame; onClose(): void }) {
  const shell = useShell();
  const remove = async () => {
    const selected = props.game;
    try {
      const games = await window.Polymer.library.games.list();
      const game = games.find(game => game.launchables.some(item => item.path === selected.path));
      if (game) await window.Polymer.library.games.delete(game.id);
      const paths = new Set(game?.launchables.map(item => item.path) ?? [selected.path]);
      shell.setModel("recent", (entries: RecentGame[]) => entries.filter(entry => !paths.has(entry.path)));
      shell.setModel("libraryGames", (entries: RecentGame[]) => entries.filter(entry => !paths.has(entry.path)));
    } catch (error) {
      shell.setModel("error", `Could not delete ${selected.name}: ${String(error)}`);
    }
  };
  return <ContextMenu title={props.game.name} description={friendly(props.game.platform)} onClose={props.onClose}
    actions={[{ label: "Delete game", danger: true, run: () => void remove() }]} />;
}
