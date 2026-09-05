import type { ParsedHotkey } from "polymer:api";

export class HotkeyParser {
    static dolphin(expression: string): ParsedHotkey {
        const text = expression.startsWith("@(") && expression.endsWith(")")
            ? expression.slice(2, -1)
            : expression;
        return this.keyboard(text.split("+"));
    }

    static pcsx2(expression: string): ParsedHotkey {
        return this.keyboard(expression.split("&").map(part => {
            const value = part.trim();
            return value.startsWith("Keyboard/") ? value.slice("Keyboard/".length) : value;
        }));
    }

    private static keyboard(parts: string[]): ParsedHotkey {
        const aliases: Readonly<Record<string, string>> = {
            alt: "Alt",
            control: "Control",
            ctrl: "Control",
            escape: "Escape",
            esc: "Escape",
            period: "Period",
            return: "Enter",
            shift: "Shift",
            space: "Space",
            tab: "Tab",
        };
        const codes = parts
            .map(part => part.trim().replaceAll("`", ""))
            .filter(Boolean)
            .map(part => {
                const alias = aliases[part.toLowerCase()];
                if (alias) return alias;
                if (/^f(?:[1-9]|1\d|2[0-4])$/i.test(part)) return part.toUpperCase();
                if (/^key[a-z]$/i.test(part)) return `Key${part.at(-1)!.toUpperCase()}`;
                if (/^digit\d$/i.test(part)) return `Digit${part.at(-1)}`;
                if (/^[a-z]$/i.test(part)) return `Key${part.toUpperCase()}`;
                if (/^\d$/.test(part)) return `Digit${part}`;
                if (/^vk_[0-9a-f]+$/i.test(part)) return part.toUpperCase();
                throw new TypeError(`unsupported keyboard hotkey key '${part}'`);
            });
        if (codes.length === 0) throw new TypeError("hotkey expression is empty");
        const code = codes.at(-1)!;
        return { kind: "keyboard", code, modifiers: codes.slice(0, -1) };
    }
}

