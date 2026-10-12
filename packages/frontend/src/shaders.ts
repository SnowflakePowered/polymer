import type { FilterChain, ShaderPreset, ShaderPresetEntry } from "./polymer";

const STORAGE_KEY = "polymer.shaders";

/** The shader chosen for a platform, as a path inside the shader folder. */
export interface PlatformShader {
  readonly preset: string | null;
  readonly enabled: boolean;
}

export interface ShaderTarget {
  attachShader(preset: ShaderPreset): FilterChain;
}

const DEFAULT_SHADER: PlatformShader = Object.freeze({ preset: null, enabled: true });

/**
 * Applies, saves and toggles the shader chosen for each platform while a game
 * is presented. Choices persist in `localStorage`, shared by every theme.
 */
export class PlatformShaders {
  #platform?: string;
  #target?: () => ShaderTarget | undefined;
  #chain?: FilterChain;
  #version = 0;

  async setting(platform: string): Promise<PlatformShader> {
    return (await PlatformShaders.#settings())[platform] ?? DEFAULT_SHADER;
  }

  /** Starts presenting `platform` on `target`, applying its saved shader. */
  async start(platform: string, target: () => ShaderTarget | undefined): Promise<PlatformShader> {
    this.#platform = platform;
    this.#target = target;
    const setting = await this.setting(platform);
    if (setting.enabled && setting.preset) await this.#attach(setting.preset);
    return setting;
  }

  stop(): void {
    this.#detach();
    this.#platform = undefined;
    this.#target = undefined;
  }

  /** Applies `entry` to the current game and saves it for its platform. */
  async apply(entry: ShaderPresetEntry): Promise<PlatformShader> {
    const setting = await this.#save({ preset: entry.relativePath, enabled: true });
    await this.#attach(entry.relativePath);
    return setting;
  }

  async setEnabled(enabled: boolean): Promise<PlatformShader> {
    const platform = this.#platform;
    if (!platform) return DEFAULT_SHADER;
    const setting = await this.#save({ ...(await this.setting(platform)), enabled });
    if (enabled && setting.preset) await this.#attach(setting.preset);
    else this.#detach();
    return setting;
  }

  async clear(): Promise<PlatformShader> {
    const setting = await this.#save({ preset: null, enabled: true });
    this.#detach();
    return setting;
  }

  async #attach(relativePath: string): Promise<void> {
    const version = ++this.#version;
    const entry = await window.Polymer.shaders.resolve(relativePath);
    if (!entry || version !== this.#version) return;
    const preset = await new ShaderPreset(entry.url).ready;
    const target = this.#target?.();
    if (version !== this.#version || !target) return;
    try {
      this.#chain = target.attachShader(preset);
    } catch (error) {
      console.error("Failed to attach shader", error);
    }
  }

  #detach(): void {
    this.#version += 1;
    this.#chain?.detach();
    this.#chain = undefined;
  }

  async #save(setting: PlatformShader): Promise<PlatformShader> {
    const platform = this.#platform;
    if (!platform) return setting;
    const settings = await PlatformShaders.#settings();
    settings[platform] = setting;
    await localStorage.setItem(STORAGE_KEY, JSON.stringify(settings));
    return setting;
  }

  static async #settings(): Promise<Record<string, PlatformShader>> {
    try {
      return JSON.parse((await localStorage.getItem(STORAGE_KEY)) ?? "{}") ?? {};
    } catch {
      return {};
    }
  }
}
