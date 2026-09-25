export type ColorMode = "light" | "dark";

export const palette = {
  light: {
    surface: "#ffffff", panel: "#f3f3f5", raised: "#ffffff", overlay: "#ffffff",
    text: "#2d2d2d", muted: "#6e6e73", action: "#1a6fd8", onAction: "#ffffff", danger: "#c43838",
    divider: "#dcdce0", track: "#c6c6cb", focus: "#14c6e0", ringStart: "#1ed8f6", ringMiddle: "#3d6bff", ringEnd: "#a55cff",
    brand: "#e60012", onBrand: "#ffffff", scrim: "rgb(0 0 0 / 36%)", shadow: "rgb(0 0 0 / 16%)",
  },
  dark: {
    surface: "#2d2d2d", panel: "#353537", raised: "#3f3f42", overlay: "#39393b",
    text: "#f1f1f1", muted: "#a9a9ae", action: "#2ecbea", onAction: "#10252b", danger: "#ff7a7a",
    divider: "#4b4b4f", track: "#5c5c61", focus: "#34e2f6", ringStart: "#1ed8f6", ringMiddle: "#3d6bff", ringEnd: "#a55cff",
    brand: "#c8000f", onBrand: "#ffffff", scrim: "rgb(0 0 0 / 55%)", shadow: "rgb(0 0 0 / 45%)",
  },
} as const;

export type Tone = keyof typeof palette.light;
