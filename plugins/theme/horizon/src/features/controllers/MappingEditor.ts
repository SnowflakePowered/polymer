import { createResource, createSignal, type Accessor } from "solid-js";
import type { InputLayout, InputMapping, MappingScope, ProjectionMode, ResolvedInputMapping } from "@polymer-fe/frontend";

export class MappingEditor {
  private readonly controllers = window.Polymer.controllers;
  private readonly resource;
  private readonly rebindState = createSignal<{ keys: readonly string[]; step: number }>();
  private readonly errorState = createSignal<string>();

  constructor(private readonly scope: Accessor<MappingScope>, private readonly layout: Accessor<InputLayout | undefined>, readonly marksChanges: Accessor<boolean>) {
    this.resource = createResource(
      () => { const layout = this.layout(); return layout ? { scope: this.scope(), layoutId: layout.id } : false; },
      ({ scope, layoutId }) => this.controllers.getMapping(scope, layoutId),
    );
  }

  private busy = false;

  readonly mapping = () => this.resource[0].state === "ready" ? this.resource[0]() : undefined;
  readonly error = () => this.errorState[0]() ?? (this.resource[0].state === "errored" ? String(this.resource[0].error?.message ?? this.resource[0].error) : undefined);
  readonly rebind = () => this.rebindState[0]();
  readonly customized = () => { const mapping = this.mapping(); return mapping !== undefined && mapping.inherited !== "default"; };

  readonly label = (key: string) => this.layout()?.elements.find(element => element.key === key)?.label ?? key;

  readonly select = (keys: readonly string[]) => this.rebindState[1]({ keys, step: 0 });
  readonly close = () => this.rebindState[1](undefined);

  private async apply(change: Promise<ResolvedInputMapping>): Promise<boolean | undefined> {
    try {
      this.resource[1].mutate(await change);
      this.errorState[1](undefined);
      return true;
    } catch (reason) {
      const message = String((reason as Error)?.message ?? reason);
      if (message.startsWith("unknown keyboard code")) return undefined;
      this.errorState[1](message);
      return false;
    }
  }

  private write(change: Partial<InputMapping>) {
    const current = this.mapping(), layout = this.layout();
    if (!current || !layout) return Promise.resolve(false as boolean | undefined);
    return this.apply(this.controllers.setMapping(this.scope(), layout.id, { mode: current.mode, orientation: current.orientation, overrides: current.overrides, ...change }));
  }

  readonly setMode = (mode: ProjectionMode | null) => void this.write({ mode });

  readonly reset = () => {
    const layout = this.layout();
    if (layout) void this.apply(this.controllers.resetMapping(this.scope(), layout.id));
  };

  readonly capture = async (bound: string) => {
    const current = this.rebind(), mapping = this.mapping();
    if (!current || !mapping || this.busy) return;
    this.busy = true;
    try {
      const ok = await this.write({ overrides: { ...mapping.overrides, [current.keys[current.step]]: bound } });
      if (ok === undefined) return;
      this.rebindState[1](ok && current.step + 1 < current.keys.length ? { keys: current.keys, step: current.step + 1 } : undefined);
    } finally {
      this.busy = false;
    }
  };
}
