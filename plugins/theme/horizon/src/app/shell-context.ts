import { createContext, useContext, type Context } from "solid-js";
import type { PackageConfiguration } from "@polymer-fe/frontend";
import type { PlatformShaders } from "@polymer-fe/frontend/shaders";
import type { LibraryQueryState, RecentGame, ShellModel } from "./models";

export type ShellContextValue = {
  model: ShellModel;
  shaders: PlatformShaders;
  setModel: (...path: any[]) => void;
  navigate(path: string, replace?: boolean): void;
  gamePath(page: "launching" | "play" | "reorder"): string;
  launchRecent(game: RecentGame): void;
  showDetails(game: RecentGame): void;
  launchDraft(draft?: PackageConfiguration): void;
  cancelDetails(): void;
  closeGame(): void;
  rememberGame(): void;
  queryLibrary(query: Partial<LibraryQueryState>): void;
  loadMoreGames(): void;
};

export const ShellContext: Context<ShellContextValue | undefined> = import.meta.hot?.data.shellContext ?? createContext<ShellContextValue>();
if (import.meta.hot) import.meta.hot.data.shellContext = ShellContext;

export function useShell() {
  const context = useContext(ShellContext);
  if (!context) throw new Error("Horizon shell context is unavailable");
  return context;
}

