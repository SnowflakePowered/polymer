import type { ParsedHotkey } from "polymer:api";

export class HotkeyParser {
    static parse(expression: string): ParsedHotkey {
        const raw = Number.parseInt(expression.trim().split(/\s+/)[0], 10);
        if (!Number.isInteger(raw) || raw < 0 || raw > 0xffff) {
            throw new TypeError(`invalid Cemu hotkey '${expression}'`);
        }
        const modifiers = [
            [1 << 13, "Alt"],
            [1 << 14, "Control"],
            [1 << 15, "Shift"],
        ].filter(([bit]) => raw & (bit as number)).map(([, name]) => name as string);
        return { kind: "keyboard", code: this.code(raw & 0x1fff), modifiers };
    }

    private static code(key: number): string {
        if (key >= 340 && key <= 363) return `F${key - 339}`;
        if (key >= 65 && key <= 90) return `Key${String.fromCharCode(key)}`;
        if (key >= 48 && key <= 57) return `Digit${String.fromCharCode(key)}`;
        const named: Readonly<Record<number, string>> = { 9: "Tab", 13: "Enter", 27: "Escape", 32: "Space" };
        if (named[key]) return named[key];
        throw new TypeError(`unsupported Cemu hotkey keycode ${key}`);
    }
}
