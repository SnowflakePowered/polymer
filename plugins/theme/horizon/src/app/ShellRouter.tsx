import { lazy } from "solid-js";
import { MemoryRouter, Route, createMemoryHistory, useNavigate } from "@solidjs/router";
import { ShellProvider } from "./ShellProvider";
import { StoragePage } from "../features/storage/StoragePage";
import { SavesPage } from "../features/saves/SavesPage";
import { SettingsPage } from "../features/settings/SettingsPage";
import { LibraryPage } from "../features/library/LibraryPage";
import { GameDetailsPage } from "../features/details/GameDetailsPage";
import { GameLayout } from "../features/game/GameLayout";
import { LaunchingPage } from "../features/game/LaunchingPage";
import { PlayPage } from "../features/game/PlayPage";
import { ControllerReorder } from "../features/game/ControllerReorder";
import "./ShellRouter.css";
import { InputProvider } from "./InputProvider";

const DevGallery = import.meta.env.DEV ? lazy(() => import("../gallery/Gallery").then(module => ({ default: module.Gallery }))) : () => null;

const history = createMemoryHistory();
history.set({ value: "/", replace: true, scroll: false });

function NotFound() {
  history.set({ value: "/", replace: true, scroll: false });
  return null;
}

export function ShellRouter() {
  return <MemoryRouter history={history} root={(props) => <InputProvider><ShellProvider>{props.children}</ShellProvider></InputProvider>}>
    <Route path="/" component={LibraryPage}>
      <Route path="/" component={() => null} />
      <Route path="/details" component={GameDetailsPage} />
    </Route>
    {import.meta.env.DEV && <Route path="/gallery" component={() => {
      const navigate = useNavigate();
      return <DevGallery onBack={() => navigate("/", { replace: true, scroll: false })} />;
    }} />}
    <Route path="/saves" component={SavesPage} />
    <Route path="/storage" component={StoragePage} />
    <Route path="/settings" component={SettingsPage} />
    <Route path="/game" component={GameLayout}>
      <Route path="/launching" component={LaunchingPage} />
      <Route path="/:platform/:game">
        <Route path="/launching" component={LaunchingPage} />
        <Route path="/play/:layout?" component={PlayPage} />
        <Route path="/reorder" component={() => <ControllerReorder />} />
      </Route>
    </Route>
    <Route path="*404" component={NotFound} />
  </MemoryRouter>;
}
