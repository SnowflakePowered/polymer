import { For, createSignal, type ParentProps } from "solid-js";
import {
  Button, Capsule, CapsuleGrid, ConfirmModal, ContextMenu, DropdownField, EmptyState, Field, FooterLegend, Glyph, Hero, IconButton, MenuItem, Modal,
  NumberField, Page, PagedSettings, PanelSection, PlayBar, PlayButton, ProgressBar, Shelf, SidePanel, SliderField, Tabs, TextField, Throbber, Toast, ToggleField, TopBar,
} from "../ui";

const GAMES = ["Super Mario Galaxy", "The Legend of Zelda: Twilight Princess", "Metroid Prime", "F-Zero GX", "Persona 4", "Okami", "Shadow of the Colossus", "Gran Turismo 4"];
const RESOLUTIONS = [{ value: "native", label: "Native (640×480)" }, { value: "2x", label: "2× (1280×960)" }, { value: "3x", label: "3× (1920×1440)" }, { value: "4x", label: "4× (2560×1920)" }, { value: "8x", label: "8× (5120×3840)", disabled: true }];

function Section(props: ParentProps<{ title: string }>) {
  return <section class="vp-gallery-section"><h2>{props.title}</h2>{props.children}</section>;
}

export function Gallery() {
  const [toggle, setToggle] = createSignal(true);
  const [slider, setSlider] = createSignal(60);
  const [resolution, setResolution] = createSignal<string | null>("2x");
  const [text, setText] = createSignal("Player 1");
  const [number, setNumber] = createSignal(4);
  const [tab, setTab] = createSignal("all");
  const [page, setPage] = createSignal("display");
  const [modal, setModal] = createSignal(false);
  const [confirm, setConfirm] = createSignal(false);
  const [menu, setMenu] = createSignal(false);
  const [panel, setPanel] = createSignal<"left" | "right" | null>(null);

  return <div class="vp-gallery">
    <TopBar title="Vapor UI gallery · F2 toggles controller glyphs" actions={[{ icon: "search", label: "Search", run: () => {} }]} />
    <Page class="vp-gallery-body" hints={[{ action: "activate", label: "Select" }, { action: "back", label: "Back" }]}>
      <Section title="Buttons">
        <div class="vp-gallery-row">
          <Button primary>Primary</Button><Button>Secondary</Button><Button danger>Delete</Button><Button disabled>Disabled</Button>
          <IconButton icon="controller" label="Controller" /><IconButton icon="settings" label="Settings" />
          <PlayButton /><PlayButton state="launching" /><PlayButton state="running" />
        </div>
      </Section>
      <Section title="Fields">
        <div class="vp-gallery-fields">
          <ToggleField label="Widescreen hack" description="Renders the game in 16:9 where supported." checked={toggle()} onChange={setToggle} />
          <ToggleField label="Disabled toggle" checked={false} disabled onChange={() => {}} />
          <SliderField label="Volume" value={slider()} step={5} format={value => `${value}%`} onChange={setSlider} />
          <SliderField label="Fast forward" value={slider()} notches={["1×", "2×", "3×", "4×"]} min={0} max={100} step={33} onChange={setSlider} />
          <DropdownField label="Internal resolution" description="Higher values sharpen 3D graphics and need a faster GPU." value={resolution()} options={RESOLUTIONS} onChange={setResolution} />
          <TextField label="Player name" value={text()} onChange={setText} />
          <NumberField label="Frame skip" value={number()} min={0} max={9} onChange={setNumber} />
          <Field label="Location" description="D:\Games\Wii\Super Mario Galaxy.rvz" />
          <Field label="A very long label that has to wrap across multiple lines inside a narrow field without pushing the control off the row">
            <Button>Browse</Button>
          </Field>
        </div>
      </Section>
      <Section title="Tabs">
        <Tabs active={tab()} onChange={setTab} tabs={[{ id: "all", title: "All games", badge: "214" }, { id: "wii", title: "Wii", badge: "38" }, { id: "ps2", title: "PlayStation 2", badge: "102" }, { id: "gc", title: "GameCube", badge: "74", disabled: true }]} />
      </Section>
      <Section title="Shelf and grid">
        <div class="vp-gallery-bleed">
          <Shelf title="Recent games"><For each={GAMES}>{(game, index) => <Capsule wide={index() === 0} title={game} subtitle="Wii" badge={index() === 2 ? "New to library" : undefined} progress={index() === 3 ? .4 : undefined} />}</For></Shelf>
          <CapsuleGrid><For each={GAMES.slice(0, 6)}>{game => <Capsule title={game} />}</For></CapsuleGrid>
        </div>
      </Section>
      <Section title="Game details">
        <div class="vp-gallery-bleed">
          <Hero title="Super Mario Galaxy">
            <PlayBar action={<PlayButton />} stats={[{ label: "Platform", value: "Wii" }, { label: "Last played", value: "Today" }]}>
              <IconButton icon="controller" label="Controller settings" /><IconButton icon="settings" label="Game settings" />
            </PlayBar>
          </Hero>
        </div>
      </Section>
      <Section title="Paged settings">
        <div class="vp-gallery-bleed vp-gallery-settings">
          <PagedSettings title="Settings" active={page()} onChange={setPage} pages={[
            { id: "display", title: "Display", icon: "display", content: () => <><ToggleField label="Automatically scale user interface" checked={toggle()} onChange={setToggle} /><DropdownField label="Preferred display" value={resolution()} options={RESOLUTIONS} onChange={setResolution} /></> },
            { id: "audio", title: "Audio", icon: "volume", content: () => <SliderField label="Volume" value={slider()} onChange={setSlider} /> },
            { id: "controller", title: "Controller", icon: "controller", content: () => <EmptyState title="No controllers" body="Connect a controller to configure it." /> },
            { id: "storage", title: "Storage", icon: "storage", separatorBefore: true, content: () => <ProgressBar label="Internal NAND" detail="412 MB of 512 MB" value={.8} /> },
          ]} />
        </div>
      </Section>
      <Section title="Overlays">
        <div class="vp-gallery-row">
          <Button onClick={() => setModal(true)}>Dialog</Button>
          <Button onClick={() => setConfirm(true)}>Confirmation</Button>
          <Button onClick={() => setMenu(true)}>Context menu</Button>
          <Button onClick={() => setPanel("left")}>Main menu</Button>
          <Button onClick={() => setPanel("right")}>Quick access</Button>
        </div>
      </Section>
      <Section title="Feedback">
        <div class="vp-gallery-row"><Throbber size="small" /><Throbber /><Throbber size="large" /></div>
        <ProgressBar label="Scraping metadata" detail="12 of 40" value={.3} />
        <ProgressBar label="Preparing" />
        <div class="vp-gallery-toasts">
          <Toast title="Controller connected" body="DualSense Wireless Controller · Player 1" icon="controller" />
          <Toast title="Installed" body="Super Mario Galaxy update 1.01" tone="success" icon="check" />
          <Toast title="Something went wrong" body="The emulator exited unexpectedly." tone="error" />
        </div>
      </Section>
      <Section title="Glyphs">
        <div class="vp-gallery-row"><For each={["a", "b", "x", "y", "lb", "rb", "polymer", "options"] as const}>{name => <Glyph name={name} />}</For></div>
      </Section>
    </Page>
    <FooterLegend menuLabel="Menu" />
    <Modal open={modal()} onClose={() => setModal(false)} title="Install update?" description="Super Mario Galaxy update 1.01 is available. Installing it takes about a minute." buttons={<><Button primary data-autofocus onClick={() => setModal(false)}>Install</Button><Button onClick={() => setModal(false)}>Not now</Button></>} />
    <ConfirmModal open={confirm()} onClose={() => setConfirm(false)} title="Delete save data?" description="This removes every save for this game. This can't be undone." confirm="Delete" danger onConfirm={() => {}} />
    <ContextMenu open={menu()} onClose={() => setMenu(false)} title="Super Mario Galaxy" actions={[{ label: "Play", run: () => {} }, { label: "Properties", run: () => {} }, { label: "Manage saves", run: () => {}, disabled: true }, { label: "Remove from library", danger: true, run: () => {} }]} />
    <SidePanel side="left" open={panel() === "left"} onClose={() => setPanel(null)}>
      <MenuItem icon="home" label="Home" active data-autofocus onClick={() => setPanel(null)} />
      <MenuItem icon="library" label="Library" onClick={() => setPanel(null)} />
      <MenuItem icon="settings" label="Settings" onClick={() => setPanel(null)} />
      <MenuItem icon="power" label="Power" onClick={() => setPanel(null)} />
    </SidePanel>
    <SidePanel side="right" open={panel() === "right"} onClose={() => setPanel(null)}>
      <h2 class="vp-quick-access-title">Super Mario Galaxy</h2>
      <PanelSection title="Game"><Button primary data-autofocus onClick={() => setPanel(null)}>Resume game</Button><Button danger>Exit game</Button></PanelSection>
      <PanelSection title="Audio"><SliderField label="Volume" bottomSeparator="none" value={slider()} onChange={setSlider} /></PanelSection>
      <PanelSection title="Display"><ToggleField label="Show FPS" checked={toggle()} onChange={setToggle} /></PanelSection>
    </SidePanel>
  </div>;
}
