import type { LibraryEntry } from "../../../../../js/library-state";
import type { LaunchInstance, LaunchRequest, PackageConfiguration } from "../../../../../js/polymer";

export type RecentGame = {
  path: string;
  name: string;
  platform: string;
  emulator: string;
  playedAt: number;
  artwork?: string;
};

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
  scrapeStatus: string;
  selectedGame: string | null;
  gamepadOpen: boolean;
  pauseOpen: boolean;
  controllerToast: { name: string; index: number } | null;
};
