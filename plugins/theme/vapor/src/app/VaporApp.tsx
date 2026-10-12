import { For, Show, createMemo, onCleanup, onMount, type ParentProps } from "solid-js";
import { MemoryRouter, Route, createMemoryHistory, useLocation } from "@solidjs/router";
import { InputActionSource } from "@polymer-fe/frontend/input-actions";
import { FooterLegend, NavigationController, NavigationProvider, Toast, TopBar, useInputScope } from "../ui";
import { HomePage } from "../pages/library/HomePage";
import { LibraryPage } from "../pages/library/LibraryPage";
import { DetailsPage } from "../pages/details/DetailsPage";
import { SettingsPage } from "../pages/settings/SettingsPage";
import { GamePage } from "../pages/game/GamePage";
import { MainMenu } from "../pages/menu/MainMenu";
import { StoragePage } from "../pages/storage/StoragePage";
import { SavesPage } from "../pages/saves/SavesPage";
import { ControllerOrderModal } from "../pages/controllers/ControllerOrderModal";
import { ShaderModal } from "../pages/game/ShaderModal";
import { JobsPage } from "../pages/jobs/JobsPage";
import { active, useJobs } from "../pages/jobs/jobs";
import { ShellProvider, useShell } from "./shell";

const history = createMemoryHistory();

function Toasts() {
  const shell = useShell();
  const visible = createMemo(() => Object.values(shell.state.notifications).filter(item => !item.dismissed));
  return <div class="vp-toasts"><For each={visible()}>{item =>
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
  return <div class="vp-root" data-playing={playing() || undefined}>
    <Show when={!playing()}><TopBar actions={[
      ...jobs().some(active) ? [{ icon: "download" as const, label: "Downloads", run: () => shell.navigate("/downloads") }] : [],
      { icon: "search", label: "Library", run: () => shell.navigate("/library") }, { icon: "settings", label: "Settings", run: () => shell.navigate("/settings") },
    ]} /></Show>
    <div class="vp-content">{props.children}</div>
    <Show when={!playing() || shell.state.menu}><FooterLegend menuLabel="Menu" onMenu={toggleMenu} /></Show>
    <MainMenu />
    <ControllerOrderModal />
    <ShaderModal />
    <Toasts />
  </div>;
}

function Root(props: ParentProps) {
  const navigation = new NavigationController();
  const source = new InputActionSource(window.Polymer, navigation);
  onMount(() => { source.start(); onCleanup(() => source.dispose()); });
  return <NavigationProvider controller={navigation}><ShellProvider><Frame>{props.children}</Frame></ShellProvider></NavigationProvider>;
}

export function VaporApp() {
  return <MemoryRouter history={history} root={Root}>
    <Route path="/" component={HomePage} />
    <Route path="/library" component={LibraryPage} />
    <Route path="/details" component={DetailsPage} />
    <Route path="/settings" component={SettingsPage} />
    <Route path="/storage" component={StoragePage} />
    <Route path="/saves" component={SavesPage} />
    <Route path="/downloads" component={JobsPage} />
    <Route path="/game" component={GamePage} />
  </MemoryRouter>;
}
