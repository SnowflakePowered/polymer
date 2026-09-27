import { createContext, useContext, type Context } from "solid-js";
import type { PackageConfiguration } from "../../../../../js/polymer";
import type { RecentGame, ShellModel } from "./models";

export type ShellContextValue = {
  model: ShellModel;
  setModel: (...path: any[]) => void;
  navigate(path: string, replace?: boolean): void;
  gamePath(page: "launching" | "play" | "reorder"): string;
  launchRecent(game: RecentGame): void;
  showDetails(game: RecentGame): void;
  launchDraft(draft?: PackageConfiguration): void;
  cancelDetails(): void;
  closeGame(): void;
  rememberGame(): void;
};

export const ShellContext: Context<ShellContextValue | undefined> = import.meta.hot?.data.shellContext ?? createContext<ShellContextValue>();
if (import.meta.hot) import.meta.hot.data.shellContext = ShellContext;

export function useShell() {
  const context = useContext(ShellContext);
  if (!context) throw new Error("Horizon shell context is unavailable");
  return context;
}

