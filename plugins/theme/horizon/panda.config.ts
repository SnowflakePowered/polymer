import { defineConfig } from "@pandacss/dev";
import { palette, type Tone } from "./src/ui/palette";

const colors = Object.fromEntries((Object.keys(palette.light) as Tone[]).map(tone => [tone, { value: { base: palette.light[tone], _dark: palette.dark[tone] } }]));

export default defineConfig({
  presets: ["@pandacss/preset-base"],
  preflight: false,
  jsxFramework: "solid",
  include: ["./src/**/*.{ts,tsx}"],
  exclude: [],
  outdir: "styled-system",
  prefix: "hz",
  cssVarRoot: ":root",
  conditions: {
    extend: {
      dark: "[data-color-mode=dark] &",
      light: "[data-color-mode=light] &",
      active: "&:is([data-active], [aria-current=page])",
      focus: "&:is(:focus, [data-focus])",
      hover: "&:is(:hover:not(:disabled), [data-hover])",
    },
  },
  theme: {
    extend: {
      tokens: {
        fonts: { body: { value: "Roboto, 'Segoe UI', Arial, sans-serif" } },
        radii: { control: { value: ".5rem" }, card: { value: ".75rem" }, panel: { value: "1rem" }, pill: { value: "999px" } },
        durations: { motion: { value: "160ms" } },
      },
      semanticTokens: { colors },
    },
  },
});
