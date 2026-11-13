const files = import.meta.glob("../../../assets/prompts/pads/*.svg", { eager: true, query: "?url", import: "default" }) as Record<string, string>;

const directions = (prefix: string, up: string, right: string, down: string, left: string, stick = false): Readonly<Record<string, string>> => ({
  "directional.n": `${prefix}dpad_${up}`, "directional.e": `${prefix}dpad_${right}`, "directional.s": `${prefix}dpad_${down}`, "directional.w": `${prefix}dpad_${left}`,
  ...stick ? {
    "analog.stick.left.y+": `${prefix}stick_up`, "analog.stick.left.y-": `${prefix}stick_down`, "analog.stick.left.x-": `${prefix}stick_left`, "analog.stick.left.x+": `${prefix}stick_right`,
    "analog.stick.right.y+": `${prefix}stick_c_up`, "analog.stick.right.y-": `${prefix}stick_c_down`, "analog.stick.right.x-": `${prefix}stick_c_left`, "analog.stick.right.x+": `${prefix}stick_c_right`,
  } : {},
});

const SETS = {
  gamecube: {
    "button.a": "gamecube_button_a", "button.b": "gamecube_button_b", "button.x": "gamecube_button_x", "button.y": "gamecube_button_y", "button.z": "gamecube_button_z", "button.start": "gamecube_button_start",
    "button.l": "gamecube_trigger_l", "button.r": "gamecube_trigger_r", "trigger.left": "gamecube_trigger_l", "trigger.right": "gamecube_trigger_r",
    ...directions("gamecube_", "up", "right", "down", "left", true),
  },
  switch: {
    "button.a": "switch_button_a", "button.b": "switch_button_b", "button.x": "switch_button_x", "button.y": "switch_button_y", "button.l": "switch_button_l", "button.r": "switch_button_r",
    "trigger.left": "switch_button_zl", "trigger.right": "switch_button_zr", "button.start": "switch_button_plus", "button.select": "switch_button_minus", "button.guide": "switch_button_home",
    ...directions("switch_", "up", "right", "down", "left"),
  },
  playstation: {
    "button.a": "playstation_button_cross", "button.b": "playstation_button_circle", "button.x": "playstation_button_square", "button.y": "playstation_button_triangle",
    "button.l": "playstation_trigger_l1", "button.r": "playstation_trigger_r1", "trigger.left": "playstation_trigger_l2", "trigger.right": "playstation_trigger_r2",
    "button.start": "playstation5_button_options", "button.select": "playstation5_button_create",
  },
  xbox: {
    "button.a": "xbox_button_a", "button.b": "xbox_button_b", "button.x": "xbox_button_x", "button.y": "xbox_button_y",
    "button.l": "xbox_lb", "button.r": "xbox_rb", "trigger.left": "xbox_lt", "trigger.right": "xbox_rt",
    "button.start": "xbox_button_menu", "button.select": "xbox_button_view", "button.guide": "xbox_guide",
  },
} as const satisfies Record<string, Readonly<Record<string, string>>>;

const LAYOUTS: Readonly<Record<string, keyof typeof SETS>> = {
  GAMECUBE_CONTROLLER: "gamecube",
  NES_CONTROLLER: "switch", FC_MIC_CONTROLLER: "switch", SNES_CONTROLLER: "switch", GB_LAYOUT: "switch", GBA_LAYOUT: "switch", VB_CONTROLLER: "switch",
  NDS_LAYOUT: "switch", DSI_LAYOUT: "switch", "3DS_LAYOUT": "switch",
  WII_REMOTE_CONTROLLER: "switch", WII_REMOTE_NUNCHUCK_CONTROLLER: "switch", WIIU_DRC_CONTROLLER: "switch", WIIU_PRO_CONTROLLER: "switch",
  SWITCH_PRO_CONTROLLER: "switch", SWITCH_2_PRO_CONTROLLER: "switch", SWITCH_JOYCONS_CONTROLLER: "switch", SWITCH_2_JOYCONS_CONTROLLER: "switch",
  SWITCH_LEFT_JOYCON_CONTROLLER: "switch", SWITCH_RIGHT_JOYCON_CONTROLLER: "switch", SWITCH_2_LEFT_JOYCON_CONTROLLER: "switch", SWITCH_2_RIGHT_JOYCON_CONTROLLER: "switch",
  PSX_CONTROLLER: "playstation", DUALSHOCK_CONTROLLER: "playstation", DUALSHOCK_2_CONTROLLER: "playstation", DUALSHOCK_3_CONTROLLER: "playstation", DUALSHOCK_4_CONTROLLER: "playstation", DUALSENSE_CONTROLLER: "playstation",
  XBOX_CONTROLLER: "xbox",
};

export function targetGlyph(layoutId: string, key: string): string | undefined {
  const family = LAYOUTS[layoutId];
  const name = family && (SETS[family] as Readonly<Record<string, string>>)[key];
  return name ? files[`../../../assets/prompts/pads/${name}.svg`] : undefined;
}
