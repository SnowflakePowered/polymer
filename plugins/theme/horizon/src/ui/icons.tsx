import type { JSX } from "solid-js";
import { useTone, type Tone } from "./theme";

export type IconName = "user" | "friends" | "network" | "library" | "saves" | "storage" | "settings" | "back" | "play" | "info" | "close" | "gamepad" | "keyboard" | "volume" | "theme" | "key" | "trash";

const paths: Record<IconName, () => JSX.Element> = {
  user: () => <><path d="M12 4a4 4 0 1 1 0 8 4 4 0 0 1 0-8Z" /><path d="M4.5 21a7.5 7.5 0 0 1 15 0" /></>,
  friends: () => <><path d="M9 5a3 3 0 1 1 0 6 3 3 0 0 1 0-6ZM17 6.5a2.5 2.5 0 1 1 0 5 2.5 2.5 0 0 1 0-5Z" /><path d="M2.5 20a6.5 6.5 0 0 1 13 0M14 15a5 5 0 0 1 7.5 4.3" /></>,
  network: () => <><path d="M12 3a9 9 0 1 1 0 18 9 9 0 0 1 0-18Z" /><path d="M3 12h18M12 3a14 14 0 0 1 0 18M12 3a14 14 0 0 0 0 18" /></>,
  library: () => <><rect x="3.5" y="3.5" width="7" height="7" rx=".57" /><rect x="13.5" y="3.5" width="7" height="7" rx=".57" /><rect x="3.5" y="13.5" width="7" height="7" rx=".57" /><rect x="13.5" y="13.5" width="7" height="7" rx=".57" /></>,
  saves: () => <><path d="M4 2.5h13l3.5 3.5v15h-17V3Z" /><path d="M7 2.5V9h9.5V2.5M7 21v-7.5h10V21M13.5 4v3.5" /></>,
  storage: () => <><path d="M5 4h14l3 11v5.5H2V15Z" /><path d="M2 15h20M6 17.5h7.5" /><circle cx="18" cy="17.75" r=".65" fill="currentColor" stroke="none" /></>,
  settings: () => <><path d="M10 2.5 9.5 5 7 6 5 5l-2.5 4 2 1.5v3l-2 1.5L5 19l2-1 2.5 1 .5 2.5h4.5L15 19l2.5-1 2 1 2.5-4-2-1.5v-3L22 9l-2.5-4-2 1L15 5l-.5-2.5Z" /><circle cx="12.25" cy="12" r="4" /></>,
  back: () => <path d="m15 5-7 7 7 7" />,
  play: () => <path d="m9 5 10 7-10 7Z" />,
  info: () => <><path d="M12 3a9 9 0 1 1 0 18 9 9 0 0 1 0-18Z" /><path d="M12 11v6M12 7h.01" /></>,
  close: () => <path d="m6 6 12 12M18 6 6 18" />,
  gamepad: () => <><path d="M7 6.5h10c2.5 0 4 2.5 4.5 6.5l1 5.5c.5 3.5-2.5 4-4.5 2L14.5 16.5h-5L6 20.5c-2 2-5 1.5-4.5-2l1-5.5C3 9 4.5 6.5 7 6.5Z" /><path d="M7 9.5v6M4 12.5h6M10.5 10h3" /><circle cx="17" cy="10.5" r="1" /><circle cx="19.5" cy="13.5" r="1" /></>,
  keyboard: () => <><rect x="2.5" y="6" width="19" height="12" rx="2" /><path d="M6 10h.01M9 10h.01M12 10h.01M15 10h.01M18 10h.01M6 13h.01M9 13h.01M12 13h.01M15 13h.01M18 13h.01M7 16h10" /></>,
  volume: () => <><path d="M4 9v6h4l5 4V5L8 9Z" /><path d="M16 9a4 4 0 0 1 0 6M18.5 6.5a8 8 0 0 1 0 11" /></>,
  theme: () => <><path d="M12 3a9 9 0 1 1 0 18 9 9 0 0 1 0-18Z" /><path d="M12 3v18a9 9 0 0 0 0-18Z" fill="currentColor" /></>,
  key: () => <><circle cx="7.5" cy="15.5" r="4.5" /><path d="m10.7 12.3 9.8-9.8M17.5 5.5l3 3M15 8l2 2" /></>,
  trash: () => <><path d="M4 6.5h16M9.5 6.5V4h5v2.5M6 6.5l1 14h10l1-14" /><path d="M10 10.5v6M14 10.5v6" /></>,
};

export function Icon(props: { name: IconName; class?: string; strokeWidth?: number; tone?: Tone }) {
  const color = useTone(() => props.tone ?? "text");
  return <>{(() => {
    const paint = color();
    return <svg class={props.class} data-icon={props.name} width="24" height="24" viewBox="0 0 24 24" fill="none" color={paint} stroke={paint} stroke-width={props.strokeWidth ?? 1.8} stroke-linecap="round" stroke-linejoin="round" aria-hidden="true">{paths[props.name]()}</svg>;
  })()}</>;
}
