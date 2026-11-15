import { For, Show, createMemo, onCleanup, onMount, type ParentProps } from "solid-js";
import { MemoryRouter, Route, createMemoryHistory, useLocation } from "@solidjs/router";
import { InputActionSource } from "@polymer-fe/frontend/input-actions";
import { NavRail, NavigationController, NavigationProvider, Toast, TopBar, useInputScope } from "../ui";
import { HomePage } from "../pages/library/HomePage";
import { LibraryPage } from "../pages/library/LibraryPage";
import { DetailsPage } from "../pages/details/DetailsPage";
import { SettingsPage } from "../pages/settings/SettingsPage";
import { GamePage } from "../pages/game/GamePage";
import { MainMenu } from "../pages/menu/MainMenu";
import { StoragePage } from "../pages/storage/StoragePage";
import { SavesPage } from "../pages/saves/SavesPage";
import { MappingPage } from "../pages/controllers/MappingPage";
import { ControllerOrderModal } from "../pages/controllers/ControllerOrderModal";
import { ShaderModal } from "../pages/game/ShaderModal";
import { SaveStatesModal } from "../pages/game/SaveStatesModal";
import { JobsPage } from "../pages/jobs/JobsPage";
import { active, useJobs } from "../pages/jobs/jobs";
import { ShellProvider, useShell } from "./shell";

const history = createMemoryHistory();

function Toasts() {
  const shell = useShell();
  const visible = createMemo(() => Object.values(shell.state.notifications).filter(item => !item.dismissed));
  return <div class="mc-toasts"><For each={visible()}>{item =>
    <Toast title={item.title} body={item.message} image={item.icon} tone={item.state === "error" ? "error" : item.state === "complete" ? "success" : "info"} onClick={() => shell.notifications.dismiss(item.id)} />
  }</For></div>;
}

function Frame(props: ParentProps) {
  const shell = useShell();
  const location = useLocation();
  const playing = () => location.pathname === "/game";
  const jobs = useJobs();
  const toggleMenu = () => shell.update("menu", (menu: string | null) => menu === "order" ? menu : menu ? null : playing() ? "quick" : "main");
  useInputScope(() => ({ root: document.body, handle: action => {
    if (action.type !== "pause" || playing()) return false;
    toggleMenu();
    return true;
  } }));
  const at = (path: string) => location.pathname === path;
  return <div class="mc-root" data-playing={playing() || undefined}>
    <Show when={!playing()}><NavRail items={[
      { icon: "home", label: "Home", active: at("/"), run: () => shell.navigate("/") },
      { icon: "library", label: "Library", active: at("/library"), run: () => shell.navigate("/library") },
      { icon: "disc", label: "Saves", active: at("/saves"), run: () => shell.navigate("/saves") },
      { icon: "storage", label: "System Storage", active: at("/storage"), run: () => shell.navigate("/storage") },
    ]} footer={[
      { icon: "download", label: "Updates", active: at("/downloads"), badge: jobs().some(active), run: () => shell.navigate("/downloads") },
      { icon: "settings", label: "Settings", active: at("/settings"), run: () => shell.navigate("/settings") },
    ]} /></Show>
    <div class="mc-main">
      <Show when={!playing()}><TopBar search={{ label: "Search your library", value: shell.state.query.text, onInput: text => { shell.query({ text }); if (!at("/library")) shell.navigate("/library"); } }} /></Show>
      <div class="mc-content">{props.children}</div>
    </div>
    <MainMenu />
    <ControllerOrderModal />
    <ShaderModal />
    <SaveStatesModal />
    <Toasts />
  </div>;
}

function Root(props: ParentProps) {
  const navigation = new NavigationController();
  const source = new InputActionSource(window.Polymer, navigation);
  onMount(() => { source.start(); onCleanup(() => source.dispose()); });
  return <NavigationProvider controller={navigation}><ShellProvider><Frame>{props.children}</Frame></ShellProvider></NavigationProvider>;
}

export function MicaApp() {
  return <MemoryRouter history={history} root={Root}>
    <Route path="/" component={HomePage} />
    <Route path="/library" component={LibraryPage} />
    <Route path="/details" component={DetailsPage} />
    <Route path="/settings" component={SettingsPage} />
    <Route path="/mapping" component={MappingPage} />
    <Route path="/storage" component={StoragePage} />
    <Route path="/saves" component={SavesPage} />
    <Route path="/downloads" component={JobsPage} />
    <Route path="/game" component={GamePage} />
  </MemoryRouter>;
}
