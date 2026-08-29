import { MemoryRouter, Route, createMemoryHistory } from "@solidjs/router";
import { ShellProvider } from "./ShellProvider";
import { LibraryPage } from "../features/library/LibraryPage";
import { GameDetailsPage } from "../features/details/GameDetailsPage";
import { GameLayout } from "../features/game/GameLayout";
import { LaunchingPage } from "../features/game/LaunchingPage";
import { PlayPage } from "../features/game/PlayPage";
import { ControllerReorder } from "../features/game/ControllerReorder";
import "./ShellRouter.css";
import { InputProvider } from "./InputProvider";

const history = createMemoryHistory();
history.set({ value: "/", replace: true, scroll: false });

function NotFound() {
  history.set({ value: "/", replace: true, scroll: false });
  return null;
}

export function ShellRouter() {
  return <MemoryRouter history={history} root={(props) => <InputProvider><ShellProvider>{props.children}</ShellProvider></InputProvider>}>
    <Route path="/" component={LibraryPage} />
    <Route path="/details" component={GameDetailsPage} />
    <Route path="/game" component={GameLayout}>
      <Route path="/launching" component={LaunchingPage} />
      <Route path="/play" component={PlayPage} />
      <Route path="/reorder" component={() => <ControllerReorder />} />
    </Route>
    <Route path="*404" component={NotFound} />
  </MemoryRouter>;
}
