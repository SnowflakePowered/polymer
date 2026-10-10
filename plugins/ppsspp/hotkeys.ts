import type { ParsedHotkey } from "polymer:api";

export class HotkeyParser {
    static parse(expression: string): ParsedHotkey {
        const match = /^1-(\d+)$/.exec(expression.trim());
        const key = match ? Number(match[1]) - 130 : 0;
        if (key < 1 || key > 12) throw new TypeError(`unsupported PPSSPP keyboard binding '${expression}'`);
        return { kind: "keyboard", code: `F${key}` };
    }
}
