import { useLocation } from "@solidjs/router";
import { useShell } from "../../app/shell";
import { MenuItem, SidePanel } from "../../ui";

export function MainMenu() {
  const shell = useShell();
  const location = useLocation();
  const go = (path: string) => { shell.update("menu", null); shell.navigate(path); };
  return <SidePanel side="left" open={shell.state.menu === "main"} onClose={() => shell.update("menu", null)} class="mc-main-menu">
    <MenuItem icon="home" label="Home" active={location.pathname === "/"} data-autofocus={location.pathname === "/"} onClick={() => go("/")} />
    <MenuItem icon="library" label="Library" active={location.pathname === "/library"} data-autofocus={location.pathname === "/library"} onClick={() => go("/library")} />
    <MenuItem icon="disc" label="Saves" active={location.pathname === "/saves"} data-autofocus={location.pathname === "/saves"} onClick={() => go("/saves")} />
    <MenuItem icon="storage" label="System Storage" active={location.pathname === "/storage"} data-autofocus={location.pathname === "/storage"} onClick={() => go("/storage")} />
    <MenuItem icon="download" label="Updates" active={location.pathname === "/downloads"} data-autofocus={location.pathname === "/downloads"} onClick={() => go("/downloads")} />
    <MenuItem icon="settings" label="Settings" active={location.pathname === "/settings"} data-autofocus={location.pathname === "/settings"} onClick={() => go("/settings")} />
  </SidePanel>;
}
