import { useNavigate, useLocation } from "@solidjs/router";
import { SidebarJobs } from "./notifications/SidebarJobs";
import { AvatarButton } from "./AvatarButton";
import { SidebarButton } from "./SidebarButton";
import { ControllerStatus } from "./ControllerStatus";
import "./AppSidebar.css";

export function AppSidebar() {
  const navigate = useNavigate();
  const location = useLocation();
  return <aside class="horizon-sidebar" aria-label="Horizon navigation">
    <AvatarButton />
    <nav class="horizon-sidebar-nav" aria-label="Library sections">
      <SidebarButton icon="user" label="Your library" active={location.pathname === "/"} onClick={() => navigate("/")} />
      <SidebarButton icon="friends" label="Friends" />
      <SidebarButton icon="network" label="Online play" />
    </nav>
    <div class="horizon-sidebar-spacer" />
    <SidebarJobs />
    <ControllerStatus />
    <SidebarButton icon="settings" label="Console storage" active={location.pathname === "/storage"} onClick={() => navigate("/storage")} />
  </aside>;
}
