import type { LibraryActivity, LibraryEntry } from "@polymer-fe/frontend/library-state";
import type { LaunchInstance, LaunchRequest, PackageConfiguration } from "@polymer-fe/frontend";
import type { ColorMode } from "../ui";

export type RecentGame = LibraryEntry;

export type LibrarySort = "title" | "platform" | "added";
export type LibraryQueryState = { text: string; platforms: string[]; sort: LibrarySort };

export type AudioState = { available: boolean; volume: number; muted: boolean };

export type LauncherState =
  | { kind: "idle" }
  | { kind: "detecting"; name: string }
  | { kind: "details" }
  | { kind: "launching"; name: string }
  | { kind: "error"; message: string };

export type ShellModel = {
  launcher: LauncherState;
  launchableId: string | null;
  draft: PackageConfiguration | null;
  request: LaunchRequest | null;
  surfaces: LaunchRequest["surfaces"];
  launch: LaunchInstance | null;
  audio: AudioState;
  error: string;
  recent: RecentGame[];
  libraryGames: LibraryEntry[];
  libraryTotal: number;
  libraryFiltered: boolean;
  libraryQuery: LibraryQueryState;
  librarySearchOpen: boolean;
  libraryActivities: LibraryActivity[];
  selectedGame: string | null;
  gamepadOpen: boolean;
  pauseOpen: boolean;
  controllerToast: { name: string; index: number } | null;
  colorMode: ColorMode;
};
