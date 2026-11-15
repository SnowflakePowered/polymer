import type { ManagedControllerDevice } from "@polymer-fe/frontend";

const files = import.meta.glob("../../../assets/prompts/pads/*.svg", { eager: true, query: "?url", import: "default" }) as Record<string, string>;

export type PadFamily = "xbox" | "playstation" | "switch" | "gamecube";

const SETS: Readonly<Record<PadFamily, Readonly<Record<string, string>>>> = {
  xbox: {
    "button.a": "xbox_button_a", "button.b": "xbox_button_b", "button.x": "xbox_button_x", "button.y": "xbox_button_y",
    "button.l": "xbox_lb", "button.r": "xbox_rb", "trigger.left": "xbox_lt", "trigger.right": "xbox_rt",
    "button.start": "xbox_button_menu", "button.select": "xbox_button_view", "button.guide": "xbox_guide",
    "button.clickl": "xbox_stick_l_press", "button.clickr": "xbox_stick_r_press",
    directional: "xbox_dpad", "analog.stick.left": "xbox_stick_l", "analog.stick.right": "xbox_stick_r",
  },
  playstation: {
    "button.a": "playstation_button_cross", "button.b": "playstation_button_circle", "button.x": "playstation_button_square", "button.y": "playstation_button_triangle",
    "button.l": "playstation_trigger_l1", "button.r": "playstation_trigger_r1", "trigger.left": "playstation_trigger_l2", "trigger.right": "playstation_trigger_r2",
    "button.start": "playstation5_button_options", "button.select": "playstation5_button_create",
    "button.clickl": "playstation_button_l3", "button.clickr": "playstation_button_r3",
    directional: "playstation_dpad", "analog.stick.left": "playstation_stick_l", "analog.stick.right": "playstation_stick_r",
  },
  switch: {
    "button.a": "switch_button_a", "button.b": "switch_button_b", "button.x": "switch_button_x", "button.y": "switch_button_y",
    "button.l": "switch_button_l", "button.r": "switch_button_r", "trigger.left": "switch_button_zl", "trigger.right": "switch_button_zr",
    "button.start": "switch_button_plus", "button.select": "switch_button_minus", "button.guide": "switch_button_home",
    "button.clickl": "switch_stick_l_press", "button.clickr": "switch_stick_r_press",
    directional: "switch_dpad", "analog.stick.left": "switch_stick_l", "analog.stick.right": "switch_stick_r",
  },
  gamecube: {
    "button.a": "gamecube_button_a", "button.b": "gamecube_button_b", "button.x": "gamecube_button_x", "button.y": "gamecube_button_y",
    "button.l": "gamecube_trigger_l", "button.r": "gamecube_button_z", "trigger.left": "gamecube_trigger_l", "trigger.right": "gamecube_trigger_r",
    "button.start": "gamecube_button_start", directional: "gamecube_dpad", "analog.stick.left": "gamecube_stick", "analog.stick.right": "gamecube_stick_c",
  },
};

const VENDORS: Readonly<Record<number, PadFamily>> = { 0x045e: "xbox", 0x054c: "playstation", 0x057e: "switch" };

const NAMES: readonly (readonly [RegExp, PadFamily])[] = [
  [/gamecube/i, "gamecube"],
  [/dualsense|dualshock|playstation|\bps[2-5]\b|sony/i, "playstation"],
  [/switch|joy-?con|nintendo|pro controller/i, "switch"],
  [/xbox|xinput/i, "xbox"],
];

export function padFamily(device?: Pick<ManagedControllerDevice, "name" | "vendorId">): PadFamily {
  if (!device) return "xbox";
  const named = NAMES.find(([pattern]) => pattern.test(device.name))?.[1];
  return named ?? (device.vendorId === null ? undefined : VENDORS[device.vendorId]) ?? "xbox";
}

export function padGlyph(family: PadFamily, source: string): string | undefined {
  const key = source.startsWith("directional.") ? "directional" : /^(analog\.stick\.(?:left|right))\./.exec(source)?.[1] ?? source;
  const name = SETS[family][key];
  return name ? files[`../../../assets/prompts/pads/${name}.svg`] : undefined;
}
