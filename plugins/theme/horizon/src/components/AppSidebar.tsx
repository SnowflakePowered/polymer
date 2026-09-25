import { NavigationRail } from "../ui";
import { useNavigate, useLocation } from "@solidjs/router";
import { css } from "../../styled-system/css";
import { SidebarJobs } from "./notifications/SidebarJobs";
import { AvatarButton } from "./AvatarButton";
import { SidebarButton } from "./SidebarButton";
import { ControllerStatus } from "./ControllerStatus";

const rail = css({
  display: "flex", width: "7rem", height: "100%", flex: "none", flexDirection: "column", alignItems: "center",
  paddingTop: "2rem", paddingBottom: "5.9rem", color: "onBrand", bg: "brand", overflowY: "auto",
  "@media (max-height: 600px)": { paddingTop: "1rem", paddingBottom: "1rem" },
  "@media (max-width: 720px)": { width: "5rem" },
});
const spacer = css({ display: "flex", minHeight: "1rem", flex: "1", alignItems: "center", justifyContent: "center" });
const nav = css({ display: "flex", flex: "none", flexDirection: "column", alignItems: "center", gap: "0.85rem" });

export function AppSidebar() {
  const navigate = useNavigate();
  const location = useLocation();
  return <NavigationRail class={rail} aria-label="Horizon navigation">
    <AvatarButton />
    <div class={spacer}><SidebarJobs /></div>
    <nav class={nav} aria-label="Library sections">
      <SidebarButton icon="library" iconSize={42} label="Your library" active={location.pathname === "/"} onClick={() => navigate("/")} />
      <SidebarButton icon="saves" iconSize={42} label="Saves" active={location.pathname === "/saves"} onClick={() => navigate("/saves")} />
      <SidebarButton icon="storage" iconSize={42} label="System Storage" active={location.pathname === "/storage"} onClick={() => navigate("/storage")} />
      <ControllerStatus />
      {import.meta.env.DEV && <SidebarButton icon="info" label="UI gallery" onClick={() => navigate("/gallery")} />}
      <SidebarButton icon="settings" iconSize={44} label="Settings" active={location.pathname === "/settings"} onClick={() => navigate("/settings")} />
    </nav>
  </NavigationRail>;
}
