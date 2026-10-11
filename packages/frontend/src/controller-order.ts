import type { ConnectionSession, ControllerMapping, ControllersApi, InputSourceSelector, ManagedControllerDevice } from "./polymer";

const DISCOVERED_LIMIT = 8;
const PLAYER_LIMIT = 8;

/**
 * A controller connection session ordered by an emulator's controller topology. Joining controllers
 * fill the first free port of the selected group. Fires `change` whenever players or candidates change.
 */
export class ControllerOrder extends EventTarget {
  session: ConnectionSession | null = null;
  /** The non-primary group that joins go to, or null for the primary group. */
  lesserGroup: string | null = null;
  readonly groups: ControllerMapping[][];
  private disposed = false;
  private committed = false;

  constructor(private controllers: ControllersApi, mappings: readonly ControllerMapping[]) {
    super();
    const grouped = new Map<string, ControllerMapping[]>();
    for (const mapping of mappings) grouped.set(mapping.target.group, [...grouped.get(mapping.target.group) ?? [], mapping]);
    this.groups = [...grouped.values()];
  }

  get players() { return this.session?.players ?? this.controllers.players; }
  get availability() { return this.controllers.availability; }
  get discovering() { return this.session?.discovery.active ?? false; }
  get slots() { return this.groups[0]?.map(mapping => mapping.gamepad) ?? this.players.map(player => player.index); }
  get candidates() {
    const assigned = new Set(this.players.flatMap(player => player.controller ? [player.controller.id] : []));
    return (this.session?.candidates ?? []).filter(candidate => !candidate.connected && !assigned.has(candidate.id)).slice(0, DISCOVERED_LIMIT);
  }

  controllerAt(index: number): ManagedControllerDevice | null {
    return this.players.find(player => player.index === index)?.controller ?? null;
  }

  async start() {
    this.controllers.addEventListener("statechange", this.changed);
    const session = this.controllers.activeConnectionSession ?? await this.controllers.startConnectionSession();
    if (this.disposed) {
      if (session.active) await session.commit();
      return;
    }
    this.session = session;
    session.addEventListener("change", this.changed);
    session.addEventListener("joinrequest", this.requested);
    this.changed();
  }

  setLesserGroup(group: string | null) {
    this.lesserGroup = group;
    this.changed();
  }

  async join(source: InputSourceSelector) {
    const group = this.groups.find(mappings => mappings[0].target.group === this.lesserGroup) ?? this.groups[0];
    const target = group?.find(mapping => !this.controllerAt(mapping.gamepad));
    const index = group ? target?.gamepad : Array.from({ length: PLAYER_LIMIT }, (_, index) => index).find(index => !this.controllerAt(index));
    if (index !== undefined) await this.session?.join(source, { index, group: target?.target.group });
  }

  async connect(id: string) {
    await this.session?.connect(id);
  }

  async commit() {
    if (this.committed) return;
    this.committed = true;
    try {
      if (this.session?.active) await this.session.commit();
    } catch (error) {
      this.committed = false;
      throw error;
    }
  }

  dispose() {
    this.disposed = true;
    this.controllers.removeEventListener("statechange", this.changed);
    this.session?.removeEventListener("change", this.changed);
    this.session?.removeEventListener("joinrequest", this.requested);
    if (!this.committed && this.session?.active) void this.session.commit();
  }

  private readonly changed = () => { this.dispatchEvent(new Event("change")); };
  private readonly requested = (event: Event) => {
    void this.join({ kind: "device", deviceId: (event as CustomEvent<ManagedControllerDevice>).detail.id })
      .catch(error => this.dispatchEvent(new CustomEvent("error", { detail: error })));
  };
}
