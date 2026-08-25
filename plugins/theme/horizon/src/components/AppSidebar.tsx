import { AvatarButton } from "./AvatarButton";
import { SidebarButton } from "./SidebarButton";
import "./AppSidebar.css";

export function AppSidebar() {
  return <aside class="horizon-sidebar" aria-label="Horizon navigation">
    <AvatarButton />
    <nav class="horizon-sidebar-nav" aria-label="Library sections">
      <SidebarButton icon="user" label="Your library" active />
      <SidebarButton icon="friends" label="Friends" />
      <SidebarButton icon="network" label="Online play" />
    </nav>
    <div class="horizon-sidebar-spacer" />
    <SidebarButton icon="settings" label="Settings" />
  </aside>;
}
