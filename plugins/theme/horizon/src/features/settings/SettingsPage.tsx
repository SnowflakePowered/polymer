import { Match, Switch, createSignal } from "solid-js";
import { css } from "../../../styled-system/css";
import { Screen, ScreenHeader, MasterDetail, CategoryNavigation, Section, SettingRow, Toggle, ActionHints, Icon } from "../../ui";
import { AppSidebar } from "../../components/AppSidebar";
import { useShell } from "../../app/shell-context";
import { KeySettings } from "./KeySettings";
import { PlatformSettings } from "./PlatformSettings";
import { PluginSettings } from "./PluginSettings";
import { SourceSettings } from "./SourceSettings";

const shellLayout = css({ display: "flex", height: "100%", color: "text", bg: "surface" });
const screen = css({ minWidth: "0", flex: "1" });
const headerIcon = css({ width: "2.4rem", height: "2.4rem", flex: "none" });

export function SettingsPage() {
  const shell = useShell();
  const [category, setCategory] = createSignal("themes");
  let panel!: HTMLElement;
  return <div class={shellLayout}><AppSidebar /><Screen class={screen} hints={[{ action: "back", label: "Back" }, { action: "activate", label: "OK" }]} onAction={action => { if (action.type !== "back") return false; shell.navigate("/"); return true; }}>
    <ScreenHeader title="Settings" icon={<Icon name="settings" class={headerIcon} strokeWidth={1.4} />} />
    <MasterDetail navigation={<CategoryNavigation label="Settings categories" value={category()} items={[{ value: "themes", label: "Themes" }, { value: "platforms", label: "Platforms" }, { value: "sources", label: "Library folders" }, { value: "keys", label: "Key settings" }, { value: "plugins", label: "Plugins" }]} onValueChange={setCategory} onEnter={() => panel.querySelector<HTMLElement>("input")?.focus()} />}>
      <section ref={panel}>
        <Switch>
          <Match when={category() === "themes"}>
            <Section title="Themes" description="Choose how Horizon looks. Basic White is used when dark mode is off.">
              <SettingRow description="Use the Basic Black theme with dark grey surfaces.">
                <Toggle label="Dark mode" checked={shell.model.colorMode === "dark"} onCheckedChange={checked => shell.setModel("colorMode", checked ? "dark" : "light")} />
              </SettingRow>
            </Section>
          </Match>
          <Match when={category() === "platforms"}><PlatformSettings /></Match>
          <Match when={category() === "sources"}><SourceSettings /></Match>
          <Match when={category() === "keys"}><KeySettings /></Match>
          <Match when={category() === "plugins"}><PluginSettings /></Match>
        </Switch>
      </section>
    </MasterDetail>
    <ActionHints />
  </Screen></div>;
}
