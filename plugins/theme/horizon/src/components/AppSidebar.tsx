import { createSignal } from "solid-js";
import { NavigationRail } from "../ui";
import { useNavigate, useLocation } from "@solidjs/router";
import { css } from "../../styled-system/css";
import { SidebarJobs } from "./notifications/SidebarJobs";
import { AvatarButton } from "./AvatarButton";
import { SidebarButton } from "./SidebarButton";

const rail = css({
  position: "relative", zIndex: "20", width: "7rem", height: "100%", flex: "none",
  "@media (max-width: 720px)": { width: "5rem" },
});
const panel = css({
  position: "absolute", top: "0", bottom: "0", left: "0", zIndex: "1", display: "flex", width: "100%", flexDirection: "column", alignItems: "center",
  paddingTop: "2rem", paddingBottom: "5.9rem", color: "onBrand", bg: "brand", overflowX: "hidden", overflowY: "auto", transition: "width 160ms ease",
  "@media (max-height: 600px)": { paddingTop: "1rem", paddingBottom: "1rem" },
  "[data-expanded] > &": { width: "max(100%, 38.5vw)", alignItems: "stretch" },
  "[data-expanded] > & .horizon-avatar": { marginInline: "1.6rem" },
});
const scrim = css({
  position: "fixed", inset: "0", zIndex: "0", bg: "rgb(0 0 0 / 35%)", backdropFilter: "blur(24px)", opacity: "0", pointerEvents: "none", transition: "opacity 160ms ease",
  "[data-expanded] > &": { opacity: "1" },
});
const spacer = css({ display: "flex", minHeight: "1rem", flex: "1", alignItems: "center", justifyContent: "center" });
const nav = css({
  display: "flex", flex: "none", flexDirection: "column", alignItems: "center", gap: "0.85rem",
  "[data-expanded] &": { alignItems: "stretch", gap: "0" },
});

export function AppSidebar() {
  const navigate = useNavigate();
  const location = useLocation();
  const [expanded, setExpanded] = createSignal(false);
  let root!: HTMLElement, pointer = false;
  return <NavigationRail ref={root} class={rail} aria-label="Horizon navigation" data-expanded={expanded() || undefined}
    onMouseDown={() => { pointer = true; setTimeout(() => pointer = false); }}
    onFocusIn={event => {
      if (pointer) return;
      const current = root.querySelector<HTMLElement>("[aria-current='page']");
      if (!expanded() && current && event.target !== current) current.focus();
      setExpanded(true);
    }} onFocusOut={() => queueMicrotask(() => setExpanded(root.contains(document.activeElement)))}>
    <div class={scrim} />
    <div class={panel}>
      <AvatarButton />
      <div class={spacer}><SidebarJobs /></div>
      <nav class={nav} aria-label="Library sections">
        <SidebarButton icon="library" iconSize={42} label="Library" active={location.pathname === "/"} onClick={() => navigate("/")} />
        <SidebarButton icon="saves" iconSize={42} label="Saves" active={location.pathname === "/saves"} onClick={() => navigate("/saves")} />
        <SidebarButton icon="storage" iconSize={42} label="System Storage" active={location.pathname === "/storage"} onClick={() => navigate("/storage")} />
        <SidebarButton icon="gamepad" iconSize={42} label="Controllers" active={location.pathname === "/controllers"} onClick={() => navigate("/controllers")} />
        <SidebarButton icon="settings" iconSize={44} label="Settings" active={location.pathname === "/settings"} onClick={() => navigate("/settings")} />
      </nav>
    </div>
  </NavigationRail>;
}
