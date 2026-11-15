const files = import.meta.glob("../../../assets/outlines/*.svg", { eager: true, query: "?raw", import: "default" }) as Record<string, string>;

export type Callout = { id: string; keys: readonly string[]; at: readonly [number, number]; side: "left" | "right" };
export type Outline = { svg: string; size: readonly [number, number]; callouts: readonly Callout[] };

const dpad = (prefix = "directional") => ["n", "w", "s", "e"].map(direction => `${prefix}.${direction}`);
const stick = (side: "left" | "right") => [`analog.stick.${side}.y+`, `analog.stick.${side}.x-`, `analog.stick.${side}.y-`, `analog.stick.${side}.x+`];
const c = (id: string, keys: string | readonly string[], x: number, y: number, side: "left" | "right"): Callout => ({ id, keys: typeof keys === "string" ? [keys] : keys, at: [x, y], side });
const outline = (name: string, size: readonly [number, number], callouts: readonly Callout[]): Outline => ({ svg: files[`../../../assets/outlines/${name}.svg`], size, callouts });

const part = (name: string, box: readonly [number, number, number, number], callouts: readonly Callout[]): Outline => ({
  svg: files[`../../../assets/outlines/${name}.svg`].replace(/viewBox="[^"]*"/, `viewBox="${box.join(" ")}"`),
  size: [box[2], box[3]],
  callouts: callouts.map(callout => ({ ...callout, at: [callout.at[0] - box[0], callout.at[1] - box[1]] as const })),
});

const NES = outline("nes", [500, 240], [
  c("dpad", dpad(), 100, 130, "left"), c("select", "button.select", 205, 158, "left"), c("start", "button.start", 257, 158, "left"),
  c("b", "button.b", 347, 158, "right"), c("a", "button.a", 410, 158, "right"),
]);

const FAMICOM = outline("famicom", [500, 240], [
  c("dpad", dpad(), 100, 142, "left"), c("select", "button.select", 213, 167, "left"), c("start", "button.start", 269, 167, "left"),
  c("b", "button.b", 359, 167, "right"), c("a", "button.a", 425, 167, "right"),
]);

const SNES = outline("snes", [500, 240], [
  c("l", "button.l", 120, 8, "left"), c("dpad", dpad(), 76, 120, "left"), c("select", "button.select", 209, 135, "left"), c("start", "button.start", 264, 135, "left"),
  c("r", "button.r", 380, 8, "right"), c("x", "button.x", 383, 82, "right"), c("y", "button.y", 337, 120, "right"), c("a", "button.a", 429, 120, "right"), c("b", "button.b", 383, 158, "right"),
]);

const GAMECUBE = outline("gamecube", [500, 370], [
  c("l", "button.l", 112, 55, "left"), c("start", "button.start", 250, 138, "left"), c("lstick", stick("left"), 127, 138, "left"), c("dpad", dpad(), 183, 228, "left"), c("cstick", stick("right"), 317, 228, "left"),
  c("r", "button.r", 388, 55, "right"), c("z", "button.z", 400, 60, "right"), c("y", "button.y", 362, 92, "right"), c("x", "button.x", 420, 128, "right"), c("a", "button.a", 373, 138, "right"), c("b", "button.b", 328, 161, "right"),
]);

const GAMEBOY = outline("gameboy", [320, 500], [
  c("dpad", dpad(), 77, 331, "left"), c("select", "button.select", 121, 399, "left"), c("start", "button.start", 168, 399, "left"),
  c("a", "button.a", 261, 318, "right"), c("b", "button.b", 215, 339, "right"),
]);

const GBA = outline("gba", [500, 302], [
  c("l", "button.l", 73, 40, "left"), c("dpad", dpad(), 79, 127, "left"), c("start", "button.start", 109, 192, "left"), c("select", "button.select", 109, 217, "left"),
  c("r", "button.r", 427, 40, "right"), c("a", "button.a", 442, 121, "right"), c("b", "button.b", 399, 134, "right"),
]);

const N64 = outline("n64", [500, 480], [
  c("l", "button.l", 170, 40, "left"), c("dpad", dpad("directional.left"), 113, 159, "left"), c("start", "button.start", 250, 173, "left"), c("stick", stick("left"), 250, 263, "left"),
  c("r", "button.r", 333, 40, "right"), c("c", dpad("directional.right"), 403, 142, "right"), c("b", "button.b", 332, 170, "right"), c("a", "button.a", 370, 203, "right"), c("z", "button.z", 270, 420, "right"),
]);

const WIIMOTE = outline("wiimote", [150, 500], [
  c("dpad", dpad(), 75, 89, "left"), c("minus", "button.select", 43, 239, "left"), c("one", "button.0", 75, 350, "left"),
  c("a", "button.a", 75, 175, "right"), c("b", "button.b", 130, 200, "right"), c("home", "button.guide", 75, 239, "right"), c("plus", "button.start", 107, 239, "right"), c("two", "button.1", 75, 411, "right"),
]);

const NUNCHUK = outline("wiimote-nunchuk", [460, 700], [
  c("c", "button.c", 167, 119, "left"), c("z", "button.z", 33, 159, "left"), c("stick", stick("left"), 133, 193, "left"),
  c("dpad", dpad(), 388, 111, "right"), c("a", "button.a", 388, 195, "right"), c("b", "button.b", 445, 230, "right"), c("minus", "button.select", 356, 259, "right"),
  c("home", "button.guide", 388, 259, "right"), c("plus", "button.start", 420, 259, "right"), c("one", "button.0", 388, 370, "right"), c("two", "button.1", 388, 432, "right"),
]);

const WIIU_DRC = outline("wiiu-gamepad", [500, 280], [
  c("zl", "trigger.left", 110, 18, "left"), c("l", "button.l", 60, 32, "left"), c("lstick", stick("left"), 62, 75, "left"), c("dpad", dpad(), 80, 130, "left"), c("select", "button.select", 99, 235, "left"), c("home", "button.guide", 250, 235, "left"),
  c("zr", "trigger.right", 390, 18, "right"), c("r", "button.r", 440, 32, "right"), c("rstick", stick("right"), 438, 75, "right"), c("x", "button.x", 420, 109, "right"),
  c("y", "button.y", 399, 130, "right"), c("a", "button.a", 441, 130, "right"), c("b", "button.b", 420, 150, "right"), c("start", "button.start", 401, 235, "right"),
]);

const WIIU_PRO = outline("wiiu-pro", [500, 340], [
  c("l", "button.l", 133, 27, "left"), c("zl", "trigger.left", 160, 55, "left"), c("lstick", stick("left"), 125, 117, "left"), c("minus", "button.select", 215, 133, "left"), c("home", "button.guide", 250, 133, "left"), c("dpad", dpad(), 175, 181, "left"),
  c("r", "button.r", 363, 27, "right"), c("zr", "trigger.right", 340, 55, "right"), c("rstick", stick("right"), 365, 117, "right"), c("plus", "button.start", 285, 133, "right"),
  c("x", "button.x", 325, 155, "right"), c("y", "button.y", 297, 181, "right"), c("a", "button.a", 353, 181, "right"), c("b", "button.b", 325, 209, "right"),
]);

const PSX = outline("playstation", [500, 340], [
  c("l1", "button.l", 135, 30, "left"), c("l2", "trigger.left", 95, 50, "left"),
  c("r1", "button.r", 365, 30, "right"), c("r2", "trigger.right", 405, 50, "right"),
  c("dpad", dpad(), 120, 133, "left"), c("select", "button.select", 221, 167, "left"),
  c("triangle", "button.y", 385, 99, "right"), c("square", "button.x", 347, 135, "right"), c("circle", "button.b", 422, 135, "right"), c("start", "button.start", 279, 167, "right"), c("cross", "button.a", 385, 172, "right"),
]);

const DUALSHOCK = outline("dualshock3", [500, 340], [
  c("l2", "trigger.left", 115, 34, "left"), c("l1", "button.l", 125, 60, "left"),
  c("r2", "trigger.right", 384, 34, "right"), c("r1", "button.r", 375, 60, "right"),
  c("dpad", dpad(), 107, 140, "left"), c("select", "button.select", 209, 141, "left"), c("guide", "button.guide", 250, 167, "left"), c("lstick", stick("left"), 179, 215, "left"),
  c("triangle", "button.y", 385, 105, "right"), c("start", "button.start", 291, 141, "right"), c("square", "button.x", 348, 141, "right"), c("circle", "button.b", 423, 141, "right"),
  c("cross", "button.a", 385, 179, "right"), c("rstick", stick("right"), 318, 215, "right"),
]);

const DUALSHOCK_4 = outline("dualshock4", [500, 340], [
  c("l2", "trigger.left", 95, 28, "left"), c("l1", "button.l", 125, 37, "left"),
  c("r2", "trigger.right", 405, 28, "right"), c("r1", "button.r", 375, 37, "right"),
  c("share", "button.select", 153, 63, "left"), c("touchpad", "button.0", 250, 83, "left"), c("dpad", dpad(), 103, 110, "left"), c("lstick", stick("left"), 175, 177, "left"), c("guide", "button.guide", 250, 177, "left"),
  c("options", "button.start", 346, 63, "right"), c("triangle", "button.y", 397, 75, "right"), c("square", "button.x", 363, 108, "right"), c("circle", "button.b", 429, 108, "right"),
  c("cross", "button.a", 397, 143, "right"), c("rstick", stick("right"), 315, 177, "right"),
]);

const DUALSENSE = outline("dualsense", [500, 340], [
  c("l2", "trigger.left", 80, 10, "left"), c("l1", "button.l", 110, 25, "left"),
  c("r2", "trigger.right", 420, 10, "right"), c("r1", "button.r", 390, 25, "right"),
  c("create", "button.select", 139, 43, "left"), c("touchpad", "button.0", 250, 80, "left"), c("dpad", dpad(), 103, 93, "left"), c("lstick", stick("left"), 173, 159, "left"), c("guide", "button.guide", 250, 162, "left"), c("mute", "button.1", 250, 185, "left"),
  c("options", "button.start", 360, 43, "right"), c("triangle", "button.y", 398, 63, "right"), c("square", "button.x", 365, 93, "right"), c("circle", "button.b", 430, 93, "right"),
  c("cross", "button.a", 398, 125, "right"), c("rstick", stick("right"), 327, 159, "right"),
]);

const SATURN = outline("saturn", [500, 300], [
  c("dpad", dpad(), 127, 140, "left"), c("start", "button.start", 250, 155, "left"), c("l", "button.l", 203, 210, "left"),
  c("z", "button.z", 405, 88, "right"), c("y", "button.y", 367, 105, "right"), c("x", "button.x", 330, 130, "right"), c("c", "button.c", 433, 131, "right"),
  c("b", "button.b", 387, 150, "right"), c("a", "button.a", 347, 179, "right"), c("r", "button.r", 297, 210, "right"),
]);

const DREAMCAST = outline("dreamcast", [500, 475], [
  c("stick", stick("left"), 88, 157, "left"), c("dpad", dpad(), 119, 255, "left"), c("start", "button.start", 250, 341, "left"), c("ltrigger", "trigger.left", 103, 453, "left"),
  c("y", "button.y", 397, 159, "right"), c("x", "button.x", 359, 197, "right"), c("b", "button.b", 435, 197, "right"), c("a", "button.a", 397, 235, "right"), c("rtrigger", "trigger.right", 397, 453, "right"),
]);

const GENESIS = outline("genesis-6", [500, 280], [
  c("mode", "button.select", 250, 100, "left"), c("start", "button.start", 250, 133, "left"), c("dpad", dpad(), 126, 143, "left"),
  c("z", "button.z", 401, 97, "right"), c("y", "button.y", 359, 112, "right"), c("x", "button.x", 320, 132, "right"), c("c", "button.c", 429, 145, "right"), c("b", "button.b", 380, 160, "right"), c("a", "button.a", 333, 185, "right"),
]);

const ATARI = outline("cx40", [500, 500], [
  c("fire", "button.a", 118, 143, "left"), c("joystick", dpad(), 250, 275, "right"),
]);

const VIRTUAL_BOY = outline("virtual-boy", [500, 430], [
  c("select", "button.select", 238, 92, "left"), c("ldpad", dpad("directional.left"), 89, 95, "left"),
  c("start", "button.start", 262, 92, "right"), c("rdpad", dpad("directional.right"), 407, 95, "right"), c("a", "button.a", 352, 123, "right"), c("b", "button.b", 308, 139, "right"),
]);

const SWITCH_PRO = outline("switch-pro", [500, 340], [
  c("l", "button.l", 110, 35, "left"), c("zl", "trigger.left", 75, 58, "left"), c("minus", "button.select", 195, 75, "left"), c("lstick", stick("left"), 133, 107, "left"),
  c("capture", "button.0", 220, 109, "left"), c("home", "button.guide", 280, 109, "left"), c("dpad", dpad(), 188, 167, "left"),
  c("r", "button.r", 390, 35, "right"), c("zr", "trigger.right", 425, 58, "right"), c("x", "button.x", 367, 72, "right"), c("plus", "button.start", 305, 75, "right"),
  c("y", "button.y", 332, 107, "right"), c("a", "button.a", 401, 107, "right"), c("b", "button.b", 367, 141, "right"), c("rstick", stick("right"), 311, 167, "right"),
]);

const SWITCH_2_PRO = outline("switch2-pro", [500, 365], [
  c("l", "button.l", 110, 38, "left"), c("zl", "trigger.left", 85, 52, "left"), c("minus", "button.select", 199, 87, "left"), c("lstick", stick("left"), 137, 121, "left"), c("capture", "button.0", 225, 121, "left"),
  c("home", "button.guide", 276, 121, "left"), c("dpad", dpad(), 193, 181, "left"), c("chat", "button.1", 250, 213, "left"), c("gl", "button.2", 53, 287, "left"),
  c("r", "button.r", 390, 38, "right"), c("zr", "trigger.right", 415, 52, "right"), c("plus", "button.start", 301, 87, "right"), c("x", "button.x", 363, 88, "right"), c("y", "button.y", 331, 121, "right"),
  c("a", "button.a", 395, 121, "right"), c("b", "button.b", 363, 152, "right"), c("rstick", stick("right"), 305, 181, "right"), c("gr", "button.3", 447, 287, "right"),
]);

const SWITCH_JOYCONS = outline("switch-joycons", [400, 460], [
  c("l", "button.l", 93, 30, "left"), c("zl", "trigger.left", 80, 55, "left"), c("minus", "button.select", 163, 75, "left"), c("lstick", stick("left"), 115, 145, "left"), c("dpad", dpad(), 115, 255, "left"), c("capture", "button.0", 115, 330, "left"),
  c("r", "button.r", 300, 30, "right"), c("zr", "trigger.right", 315, 55, "right"), c("plus", "button.start", 235, 75, "right"), c("x", "button.x", 285, 115, "right"), c("y", "button.y", 253, 145, "right"),
  c("a", "button.a", 315, 145, "right"), c("b", "button.b", 285, 175, "right"), c("rstick", stick("right"), 285, 255, "right"), c("home", "button.guide", 285, 330, "right"),
]);

const JOYCON_LEFT = [
  c("zl", "trigger.left", 80, 55, "left"), c("l", "button.l", 93, 30, "left"), c("minus", "button.select", 163, 75, "left"), c("sl", "button.0", 48, 105, "left"), c("sr", "button.1", 42, 135, "left"),
  c("lstick", stick("left"), 115, 145, "left"), c("dpad", dpad(), 115, 255, "left"), c("capture", "button.guide", 115, 330, "left"),
];

const JOYCON_RIGHT = [
  c("zr", "trigger.right", 315, 55, "right"), c("r", "button.r", 300, 30, "right"), c("plus", "button.start", 235, 75, "right"), c("sl", "button.0", 352, 105, "right"), c("sr", "button.1", 358, 135, "right"),
  c("x", "button.x", 285, 115, "right"), c("y", "button.y", 253, 145, "right"), c("a", "button.a", 315, 145, "right"), c("b", "button.b", 285, 175, "right"),
  c("rstick", stick("right"), 285, 255, "right"), c("home", "button.guide", 285, 330, "right"),
];

const JOYCON_LEFT_SINGLE = part("switch-joycon-left", [30, 20, 175, 450], JOYCON_LEFT);
const JOYCON_RIGHT_SINGLE = part("switch-joycon-right", [200, 20, 175, 450], JOYCON_RIGHT);

const JOYCON_2_LEFT = [
  c("zl", "trigger.left", 95, 32, "left"), c("l", "button.l", 65, 40, "left"), c("minus", "button.select", 105, 58, "left"), c("sl", "button.0", 134, 90, "left"), c("sr", "button.1", 134, 230, "left"),
  c("lstick", stick("left"), 75, 103, "left"), c("dpad", dpad(), 75, 181, "left"), c("capture", "button.guide", 99, 238, "left"),
];

const JOYCON_2_RIGHT = [
  c("zr", "trigger.right", 205, 32, "right"), c("r", "button.r", 235, 40, "right"), c("plus", "button.start", 193, 58, "right"), c("sr", "button.1", 162, 90, "right"), c("sl", "button.0", 162, 230, "right"),
  c("x", "button.x", 223, 80, "right"), c("y", "button.y", 201, 103, "right"), c("a", "button.a", 247, 103, "right"), c("b", "button.b", 223, 126, "right"),
  c("rstick", stick("right"), 224, 181, "right"), c("c", "button.2", 200, 236, "right"), c("home", "button.guide", 200, 267, "right"),
];

const SWITCH_2_JOYCONS = outline("switch2-joycons", [300, 340], [
  c("zl", "trigger.left", 95, 32, "left"), c("l", "button.l", 65, 40, "left"), c("minus", "button.select", 105, 58, "left"), c("lstick", stick("left"), 75, 103, "left"), c("dpad", dpad(), 75, 181, "left"), c("capture", "button.0", 99, 238, "left"),
  c("zr", "trigger.right", 205, 32, "right"), c("r", "button.r", 235, 40, "right"), c("plus", "button.start", 193, 58, "right"), c("x", "button.x", 223, 80, "right"), c("y", "button.y", 201, 103, "right"), c("a", "button.a", 247, 103, "right"),
  c("b", "button.b", 223, 126, "right"), c("rstick", stick("right"), 224, 181, "right"), c("c", "button.1", 200, 236, "right"), c("home", "button.guide", 200, 267, "right"),
]);

const JOYCON_2_LEFT_SINGLE = part("switch2-joycon-left", [20, 20, 130, 300], JOYCON_2_LEFT);
const JOYCON_2_RIGHT_SINGLE = part("switch2-joycon-right", [155, 20, 125, 300], JOYCON_2_RIGHT);

const XBOX = outline("xbox-one", [500, 360], [
  c("l2", "trigger.left", 110, 18, "left"), c("l1", "button.l", 87, 32, "left"), c("guide", "button.guide", 250, 53, "left"), c("lstick", stick("left"), 125, 101, "left"), c("view", "button.select", 215, 101, "left"), c("dpad", dpad(), 183, 179, "left"),
  c("r2", "trigger.right", 390, 18, "right"), c("r1", "button.r", 413, 32, "right"), c("y", "button.y", 375, 68, "right"), c("menu", "button.start", 285, 101, "right"),
  c("x", "button.x", 341, 101, "right"), c("b", "button.b", 405, 101, "right"), c("a", "button.a", 375, 135, "right"), c("rstick", stick("right"), 315, 175, "right"),
]);

const DS = outline("ds-lite", [500, 510], [
  c("l", "button.l", 50, 257, "left"), c("dpad", dpad(), 77, 354, "left"),
  c("r", "button.r", 449, 257, "right"), c("a", "button.a", 449, 344, "right"), c("b", "button.b", 423, 370, "right"), c("start", "button.start", 391, 427, "right"), c("select", "button.select", 391, 454, "right"),
]);

const CTR = outline("3ds", [500, 510], [
  c("l", "button.l", 27, 257, "left"), c("pad", stick("left"), 83, 327, "left"), c("dpad", dpad(), 78, 411, "left"), c("select", "button.select", 177, 469, "left"),
  c("r", "button.r", 473, 257, "right"), c("a", "button.a", 453, 345, "right"), c("b", "button.b", 418, 370, "right"), c("start", "button.start", 323, 469, "right"),
]);

export const outlines: Readonly<Record<string, Outline>> = {
  A2600_JOYSTICK_CONTROLLER: ATARI,
  "3DS_LAYOUT": CTR,
  DC_CONTROLLER: DREAMCAST,
  DSI_LAYOUT: DS,
  DUALSENSE_CONTROLLER: DUALSENSE,
  DUALSHOCK_CONTROLLER: DUALSHOCK,
  DUALSHOCK_2_CONTROLLER: DUALSHOCK,
  DUALSHOCK_3_CONTROLLER: DUALSHOCK,
  DUALSHOCK_4_CONTROLLER: DUALSHOCK_4,
  FC_MIC_CONTROLLER: FAMICOM,
  GAMECUBE_CONTROLLER: GAMECUBE,
  GBA_LAYOUT: GBA,
  GB_LAYOUT: GAMEBOY,
  GEN6_CONTROLLER: GENESIS,
  N64_CONTROLLER: N64,
  NDS_LAYOUT: DS,
  NES_CONTROLLER: NES,
  PSX_CONTROLLER: PSX,
  SAT_CONTROLLER: SATURN,
  SNES_CONTROLLER: SNES,
  SWITCH_2_JOYCONS_CONTROLLER: SWITCH_2_JOYCONS,
  SWITCH_2_LEFT_JOYCON_CONTROLLER: JOYCON_2_LEFT_SINGLE,
  SWITCH_2_PRO_CONTROLLER: SWITCH_2_PRO,
  SWITCH_2_RIGHT_JOYCON_CONTROLLER: JOYCON_2_RIGHT_SINGLE,
  SWITCH_LEFT_JOYCON_CONTROLLER: JOYCON_LEFT_SINGLE,
  SWITCH_RIGHT_JOYCON_CONTROLLER: JOYCON_RIGHT_SINGLE,
  SWITCH_JOYCONS_CONTROLLER: SWITCH_JOYCONS,
  SWITCH_PRO_CONTROLLER: SWITCH_PRO,
  VB_CONTROLLER: VIRTUAL_BOY,
  WIIU_DRC_CONTROLLER: WIIU_DRC,
  WIIU_PRO_CONTROLLER: WIIU_PRO,
  WII_REMOTE_CONTROLLER: WIIMOTE,
  WII_REMOTE_NUNCHUCK_CONTROLLER: NUNCHUK,
  XBOX_CONTROLLER: XBOX,
};
