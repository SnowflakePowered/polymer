import type {
    ConfigurationGroup,
    ConfigurationLayoutOption,
    DirEntry,
    DirectoryResource,
    UserConfiguration,
} from "../plugin-api.ts";

export type GraphicsApi = "vulkan" | "opengl";

type Scalar = number | string;

type Preset = {
    name: string,
    category: string,
    condition?: string,
    default: boolean,
    values: Record<string, Scalar>,
};

export type GraphicsPack = {
    rulesPath: string,
    title: string,
    description?: string,
    titleIds: string[],
    renderer?: GraphicsApi,
    defaults: Record<string, Scalar>,
    presets: Preset[],
};

type ParsedSection = { name: string, values: Record<string, string> };

export async function loadGraphicsPacks(root: DirectoryResource): Promise<GraphicsPack[]> {
    const rules: Array<{ path: string, file: Extract<DirEntry, { kind: "file" }> }> = [];
    const visit = async (directory: AsyncIterable<DirEntry>, prefix: string) => {
        for await (const entry of directory) {
            const path = prefix ? `${prefix}/${entry.name}` : entry.name;
            if (entry.kind === "directory") await visit(entry, path);
            else if (entry.name.toLowerCase() === "rules.txt") rules.push({ path, file: entry });
        }
    };
    await visit(root, "");
    rules.sort((left, right) => left.path.localeCompare(right.path));

    const packs: GraphicsPack[] = [];
    for (const rule of rules) {
        try {
            packs.push(parseGraphicsPack(rule.path, await rule.file.text()));
        } catch (error) {
            console.warn(`Cemu graphics pack '${rule.path}' was skipped:`, error);
        }
    }
    return packs;
}

export function matchingPacks(packs: GraphicsPack[], titleId: string | undefined, api: GraphicsApi) {
    const normalized = normalizeTitleId(titleId);
    if (!normalized) return [];
    return packs.filter((pack) =>
        pack.titleIds.includes(normalized) && (pack.renderer === undefined || pack.renderer === api)
    );
}

export function settleGraphicsPacks(
    packs: GraphicsPack[],
    titleId: string | undefined,
    userConfiguration: UserConfiguration | undefined,
    api: GraphicsApi,
): {
    groups: ConfigurationGroup[],
    values: Record<string, Record<string, boolean | number | string | null>>,
    entries: Array<{ filename: string, presets: Array<{ category: string, preset: string }> }>,
} {
    const groups: ConfigurationGroup[] = [];
    const values: Record<string, Record<string, boolean | number | string | null>> = {};
    const entries: Array<{ filename: string, presets: Array<{ category: string, preset: string }> }> = [];
    for (const pack of matchingPacks(packs, titleId, api)) {
        const selected = userConfiguration?.[pack.rulesPath];
        const enabled = selected?.enabled === true;
        const options: ConfigurationLayoutOption[] = [{
            key: "enabled",
            title: "Enabled",
            type: "bool",
            default: false,
        }];
        const settled: Record<string, boolean | number | string | null> = { enabled };
        const presets: Array<{ category: string, preset: string }> = [];
        if (enabled) {
            const variables = { ...pack.defaults };
            for (const category of categories(pack)) {
                const available = pack.presets.filter((preset, index, all) =>
                    preset.category === category
                    && evaluateCondition(preset.condition, variables)
                    && all.findIndex((candidate) =>
                        candidate.category === category && candidate.name === preset.name
                    ) === index
                );
                if (available.length === 0) continue;
                const fallback = available.find((preset) => preset.default) ?? available[0];
                const requested = selected?.[optionKey(category)];
                const chosen = typeof requested === "string"
                    ? available.find((preset) => preset.name === requested) ?? fallback
                    : fallback;
                options.push({
                    key: optionKey(category),
                    title: category || "Preset",
                    type: "select",
                    default: fallback.name,
                    choices: available.map((preset) => ({ key: preset.name, label: preset.name })),
                });
                settled[optionKey(category)] = chosen.name;
                presets.push({ category, preset: chosen.name });
                Object.assign(variables, chosen.values);
            }
            entries.push({
                filename: `graphicPacks/downloadedGraphicPacks/${pack.rulesPath}`,
                presets,
            });
        }
        groups.push({
            key: pack.rulesPath,
            title: pack.title,
            description: pack.description,
            options,
        });
        values[pack.rulesPath] = settled;
    }
    return { groups, values, entries };
}

function parseGraphicsPack(rulesPath: string, source: string): GraphicsPack {
    const sections: ParsedSection[] = [];
    let current: ParsedSection | undefined;
    for (const rawLine of source.replaceAll("\r\n", "\n").split("\n")) {
        const line = stripComment(rawLine).trim();
        if (!line) continue;
        const heading = line.match(/^\[([^\]]+)\]$/);
        if (heading) {
            current = { name: heading[1], values: {} };
            sections.push(current);
            continue;
        }
        const equals = line.indexOf("=");
        if (!current) throw new Error(`value outside a section: '${rawLine.trim()}'`);
        if (equals < 1) continue;
        current.values[line.slice(0, equals).trim()] = line.slice(equals + 1).trim();
    }
    const definition = sections.find((section) => section.name.toLowerCase() === "definition")?.values;
    if (!definition) throw new Error("missing [Definition]");
    const titleIds = (field(definition, "titleIds") || "").split(",").map(normalizeTitleId).filter((value): value is string => Boolean(value));
    const displayPath = unquote(field(definition, "path") || rulesPath.replace(/\/rules\.txt$/i, ""));
    if (!displayPath) throw new Error("missing Definition.path");
    const renderer = unquote(field(definition, "rendererFilter") || "").toLowerCase();
    if (renderer && renderer !== "vulkan" && renderer !== "opengl") {
        throw new Error(`unsupported rendererFilter '${renderer}'`);
    }
    const defaults = variableValues(sections.find((section) => section.name.toLowerCase() === "default")?.values || {});
    const presets = sections
        .filter((section) => section.name.toLowerCase() === "preset")
        .map((section) => {
            const name = unquote(field(section.values, "name") || "");
            if (!name) throw new Error("preset is missing a name");
            return {
                name,
                category: unquote(field(section.values, "category") || ""),
                condition: field(section.values, "condition"),
                default: parseScalar(field(section.values, "default") || "0") === 1,
                values: variableValues(section.values),
            };
        });
    return {
        rulesPath,
        title: unquote(field(definition, "name") || displayPath),
        description: field(definition, "description") ? unquote(field(definition, "description")!).replaceAll("|", "\n") : undefined,
        titleIds,
        renderer: renderer as GraphicsApi || undefined,
        defaults,
        presets,
    };
}

function categories(pack: GraphicsPack) {
    return [...new Set(pack.presets.map((preset) => preset.category))];
}

function optionKey(category: string) {
    return category || "uncategorizedPreset";
}

function variableValues(values: Record<string, string>) {
    return Object.fromEntries(Object.entries(values).flatMap(([rawKey, value]) => {
        if (!rawKey.startsWith("$")) return [];
        return [[rawKey.slice(1).split(":", 1)[0], parseScalar(value)]];
    }));
}

function parseScalar(value: string): Scalar {
    const text = unquote(value.trim());
    if (/^-?0x[0-9a-f]+$/i.test(text)) {
        const negative = text.startsWith("-");
        const number = Number.parseInt(text.replace("-", ""), 16);
        return negative ? -number : number;
    }
    const number = Number(text);
    return Number.isFinite(number) ? number : text;
}

function stripComment(line: string) {
    let quoted = false;
    for (let index = 0; index < line.length; index++) {
        if (line[index] === '"' && line[index - 1] !== "\\") quoted = !quoted;
        if (!quoted && (line[index] === "#" || (line[index] === "/" && line[index + 1] === "/"))) return line.slice(0, index);
    }
    return line;
}

function field(values: Record<string, string>, name: string) {
    const found = Object.keys(values).find((key) => key.toLowerCase() === name.toLowerCase());
    return found === undefined ? undefined : values[found];
}

function unquote(value: string) {
    const trimmed = value.trim();
    return trimmed.startsWith('"') && trimmed.endsWith('"')
        ? trimmed.slice(1, -1).replaceAll('\\"', '"')
        : trimmed;
}

export function normalizeTitleId(value: string | undefined) {
    const normalized = value?.replace(/[^0-9a-f]/gi, "").toUpperCase();
    return normalized || undefined;
}

function evaluateCondition(condition: string | undefined, variables: Record<string, Scalar>) {
    if (!condition) return true;
    try {
        return new Expression(condition, variables).evaluate() !== 0;
    } catch {
        return false;
    }
}

class Expression {
    readonly #tokens: string[];
    #index = 0;

    constructor(source: string, private readonly variables: Record<string, Scalar>) {
        this.#tokens = source.match(/\$[A-Za-z_][A-Za-z0-9_]*|-?0x[0-9a-f]+|(?:\d+\.\d*|\.\d+|\d+)|==|!=|<=|>=|[()+\-*/<>]/gi) || [];
        if (this.#tokens.join("").length !== source.replace(/\s+/g, "").length) throw new Error("unsupported condition token");
    }

    evaluate() {
        const value = this.comparison();
        if (this.#index !== this.#tokens.length) throw new Error("trailing condition tokens");
        return value;
    }

    private comparison(): number {
        let left = this.sum();
        while (["==", "!=", "<", ">", "<=", ">="].includes(this.peek())) {
            const operator = this.take();
            const right = this.sum();
            left = Number(operator === "==" ? left === right : operator === "!=" ? left !== right : operator === "<" ? left < right : operator === ">" ? left > right : operator === "<=" ? left <= right : left >= right);
        }
        return left;
    }

    private sum(): number {
        let value = this.product();
        while (["+", "-"].includes(this.peek())) value = this.take() === "+" ? value + this.product() : value - this.product();
        return value;
    }

    private product(): number {
        let value = this.unary();
        while (["*", "/"].includes(this.peek())) value = this.take() === "*" ? value * this.unary() : value / this.unary();
        return value;
    }

    private unary(): number {
        if (this.peek() === "+") { this.take(); return this.unary(); }
        if (this.peek() === "-") { this.take(); return -this.unary(); }
        return this.primary();
    }

    private primary(): number {
        const token = this.take();
        if (token === "(") {
            const value = this.comparison();
            if (this.take() !== ")") throw new Error("unclosed condition group");
            return value;
        }
        if (token.startsWith("$")) {
            const value = this.variables[token.slice(1)] ?? 0;
            return typeof value === "number" ? value : Number(value) || 0;
        }
        return Number(parseScalar(token));
    }

    private peek() { return this.#tokens[this.#index] || ""; }
    private take() {
        const token = this.#tokens[this.#index++];
        if (!token) throw new Error("unexpected end of condition");
        return token;
    }
}
