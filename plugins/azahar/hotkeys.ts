import type { ParsedHotkey } from "polymer:api";

export class HotkeyParser {
    static parse(expression: string): ParsedHotkey {
        const aliases: Readonly<Record<string, string>> = {
            alt: "Alt",
            ctrl: "Control",
            esc: "Escape",
            escape: "Escape",
            return: "Enter",
            shift: "Shift",
            space: "Space",
            tab: "Tab",
        };
        const codes = expression.split("+").map(part => part.trim()).filter(Boolean).map(part => {
            const alias = aliases[part.toLowerCase()];
            if (alias) return alias;
            if (/^f(?:[1-9]|1\d|2[0-4])$/i.test(part)) return part.toUpperCase();
            if (/^[a-z]$/i.test(part)) return `Key${part.toUpperCase()}`;
            if (/^\d$/.test(part)) return `Digit${part}`;
            throw new TypeError(`unsupported keyboard hotkey key '${part}'`);
        });
        if (codes.length === 0) throw new TypeError("hotkey expression is empty");
        return { kind: "keyboard", code: codes.at(-1)!, modifiers: codes.slice(0, -1) };
    }
}
