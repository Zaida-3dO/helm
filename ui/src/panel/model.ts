// The side panel's tab host as plain data: sets, caps, LRU eviction, intent, selection and closing.
// No React and no DOM, so every rule is unit-tested.

export type Intent = 'user' | 'program';
export type CloseReason = 'user' | 'evicted';
export type SlotId = 'changes' | 'files';

/** What the model needs to know about a kind; the full kind (render, icon...) lives in the registry. */
export interface KindRules {
  scope: 'chat' | 'app';
  /** Per set, for this kind. */
  maxTabs?: number;
  /** Default true. A non-evictable tab refuses opens that would need its room. */
  evictable?: boolean;
  defaultMaximized?: boolean | ((data: unknown) => boolean);
  onClose?: (tab: Readonly<PanelTab>, reason: CloseReason) => void;
}

export interface PanelTab {
  id: string;
  kind: string;
  key: string;
  data: unknown;
  title?: string;
  /** Use stamp: higher is more recent. */
  used: number;
  /** The set: a chat id, '' (no chat) or 'app'. */
  set: string;
  intent: Intent;
}

export interface OpenOptions {
  intent: Intent;
  chatId?: string | null;
}

export const SET_CAP = 12;
export const TOTAL_CAP = 48;
export const APP_SET = 'app';
export const SLOTS: readonly SlotId[] = ['changes', 'files'];

const slotEntry = (slot: SlotId) => `slot:${slot}`;
const entrySlot = (entry: string): SlotId | null =>
  entry === 'slot:changes' ? 'changes' : entry === 'slot:files' ? 'files' : null;

export class PanelModel {
  /** Every open tab, in the one global strip order. */
  readonly tabs: PanelTab[] = [];
  /** The view (a chat id or '') a selection belongs to → entry id (a tab id or `slot:<id>`). */
  private readonly selected = new Map<string, string>();
  /** When each view last selected each slot. */
  private readonly slotUsed = new Map<string, Map<SlotId, number>>();
  private seq = 0;
  private nextId = 1;
  hidden = false;
  maximized = false;
  /** The Inspector (Changes / Files slots) is shown. */
  inspector = false;

  private readonly rules: (kind: string) => KindRules | undefined;

  constructor(rules: (kind: string) => KindRules | undefined) {
    this.rules = rules;
  }

  private tick(): number {
    return ++this.seq;
  }

  static view(chatId: string | null | undefined): string {
    return chatId ?? '';
  }

  /** The tabs a view shows: its own set plus the app set, in strip order. */
  displayed(chatId: string | null): PanelTab[] {
    const view = PanelModel.view(chatId);
    return this.tabs.filter((t) => t.set === APP_SET || t.set === view);
  }

  /** Only user-intent tabs count towards showing the panel. */
  counted(chatId: string | null): PanelTab[] {
    return this.displayed(chatId).filter((t) => t.intent === 'user');
  }

  visible(chatId: string | null): boolean {
    return this.inspector || (this.counted(chatId).length > 0 && !this.hidden);
  }

  /** The entry the strip shows as selected: a tab id, `slot:<id>`, or null. */
  activeEntry(chatId: string | null): string | null {
    const view = PanelModel.view(chatId);
    const remembered = this.selected.get(view);
    if (remembered) {
      const slot = entrySlot(remembered);
      if (slot && this.inspector) return remembered;
      if (!slot && this.counted(chatId).some((t) => t.id === remembered)) return remembered;
    }
    // Fallback: the strip's earliest qualifying entry. Program tabs never qualify.
    if (this.inspector) return slotEntry(SLOTS[0] as SlotId);
    return this.counted(chatId)[0]?.id ?? null;
  }

  activeTab(chatId: string | null): PanelTab | null {
    const entry = this.activeEntry(chatId);
    return entry ? (this.tabs.find((t) => t.id === entry) ?? null) : null;
  }

  private setFor(rules: KindRules, chatId: string | null | undefined, current: string | null): string {
    if (rules.scope === 'app') return APP_SET;
    return PanelModel.view(chatId ?? current);
  }

  private evictable(t: PanelTab): boolean {
    return this.rules(t.kind)?.evictable !== false;
  }

  private static lru(list: PanelTab[]): PanelTab | null {
    let best: PanelTab | null = null;
    for (const t of list) if (!best || t.used < best.used) best = t;
    return best;
  }

  /**
   * Opens (or updates) a tab. Returns the tab, or null when the open was refused: the kind is
   * unknown, or no room can be made. A refused open changes nothing; callers fall back.
   */
  open(kind: string, key: string, data: unknown, opts: OpenOptions, current: string | null): PanelTab | null {
    const rules = this.rules(kind);
    if (!rules) return null;
    const set = this.setFor(rules, opts.chatId, current);
    const existing = this.tabs.find((t) => t.set === set && t.kind === kind && t.key === key);
    if (existing) {
      existing.data = data;
      if (opts.intent === 'user') this.showByUser(existing, rules, current);
      return existing;
    }

    // Work out every eviction first, so a refusal leaves the model untouched.
    const shownId = this.visible(current) ? this.activeEntry(current) : null;
    const eligible = (t: PanelTab) =>
      this.evictable(t) && (opts.intent === 'user' || (t.intent === 'program' && t.id !== shownId));
    const doomed: PanelTab[] = [];
    const alive = () => this.tabs.filter((t) => !doomed.includes(t));
    const makeRoom = (pool: PanelTab[], preferred?: (t: PanelTab) => boolean): boolean => {
      const candidates = pool.filter(eligible);
      const pick = (preferred && PanelModel.lru(candidates.filter(preferred))) || PanelModel.lru(candidates);
      if (!pick) return false;
      doomed.push(pick);
      return true;
    };

    if (rules.maxTabs !== undefined) {
      const sameKind = alive().filter((t) => t.set === set && t.kind === kind);
      if (sameKind.length >= rules.maxTabs && !makeRoom(sameKind)) return null;
    }
    const inSet = alive().filter((t) => t.set === set);
    if (inSet.length >= SET_CAP && !makeRoom(inSet)) return null;
    const all = alive();
    if (all.length >= TOTAL_CAP) {
      const view = PanelModel.view(current);
      // For the total cap, tabs the current chat does not display go first.
      const notDisplayed = (t: PanelTab) => t.set !== APP_SET && t.set !== view;
      if (!makeRoom(all, notDisplayed)) return null;
    }

    for (const t of doomed) this.close(t.id, 'evicted');
    const tab: PanelTab = { id: `tab-${this.nextId++}`, kind, key, data, used: this.tick(), set, intent: opts.intent };
    this.tabs.push(tab);
    if (opts.intent === 'user') this.showByUser(tab, rules, current);
    return tab;
  }

  /**
   * Brings back a saved tab as program intent in exactly its saved set. Returns false
   * when it would not fit under the caps or is already open; nothing is evicted for a restore.
 */
  restore(set: string, kind: string, key: string, data: unknown): boolean {
    const rules = this.rules(kind);
    if (!rules || (rules.scope === 'app') !== (set === APP_SET)) return false;
    if (this.tabs.some((t) => t.set === set && t.kind === kind && t.key === key)) return false;
    const inSet = this.tabs.filter((t) => t.set === set);
    const sameKind = inSet.filter((t) => t.kind === kind);
    if (this.tabs.length >= TOTAL_CAP || inSet.length >= SET_CAP) return false;
    if (rules.maxTabs !== undefined && sameKind.length >= rules.maxTabs) return false;
    this.tabs.push({ id: `tab-${this.nextId++}`, kind, key, data, used: 0, set, intent: 'program' });
    return true;
  }

  private showByUser(tab: PanelTab, rules: KindRules, current: string | null): void {
    tab.intent = 'user';
    tab.used = this.tick();
    // A chat tab is selected in its own chat; an app tab in the chat being viewed.
    this.selected.set(tab.set === APP_SET ? PanelModel.view(current) : tab.set, tab.id);
    this.hidden = false;
    const max = typeof rules.defaultMaximized === 'function' ? rules.defaultMaximized(tab.data) : rules.defaultMaximized;
    if (max) this.maximized = true;
  }

  /** Selecting a program tab upgrades it to user intent. */
  select(entry: string, current: string | null): void {
    const view = PanelModel.view(current);
    const slot = entrySlot(entry);
    if (slot) {
      if (!this.inspector) return;
      let m = this.slotUsed.get(view);
      if (!m) this.slotUsed.set(view, (m = new Map()));
      m.set(slot, this.tick());
      this.selected.set(view, entry);
      return;
    }
    const tab = this.displayed(current).find((t) => t.id === entry);
    if (!tab) return;
    tab.intent = 'user';
    tab.used = this.tick();
    this.selected.set(view, tab.id);
    this.hidden = false;
  }

  /**
   * Closes a tab. `onClose` fires exactly once per tab, and only here (never on hide or unmount).
   * Every view that had it selected moves to its most recently used counted tab or slot.
   */
  close(id: string, reason: CloseReason): boolean {
    const index = this.tabs.findIndex((t) => t.id === id);
    if (index < 0) return false;
    const [tab] = this.tabs.splice(index, 1) as [PanelTab];
    for (const [view, entry] of [...this.selected]) {
      if (entry !== id) continue;
      const next = this.mostRecent(view === '' ? null : view);
      if (next) this.selected.set(view, next);
      else this.selected.delete(view);
    }
    if (!this.tabs.some((t) => t.intent === 'user')) this.maximized = false;
    try {
      this.rules(tab.kind)?.onClose?.(tab, reason);
    } catch (e) {
      console.error(`panel kind ${tab.kind} onClose failed`, e);
    }
    return true;
  }

  private mostRecent(chatId: string | null): string | null {
    let best: { entry: string; used: number } | null = null;
    for (const t of this.counted(chatId)) if (!best || t.used > best.used) best = { entry: t.id, used: t.used };
    if (this.inspector) {
      const m = this.slotUsed.get(PanelModel.view(chatId));
      for (const slot of SLOTS) {
        const used = m?.get(slot) ?? 0;
        if (!best || used > best.used) best = { entry: slotEntry(slot), used };
      }
    }
    return best?.entry ?? null;
  }

  /** Closes every tab of a set (a deleted chat); reason `evicted`. */
  dropSet(set: string): void {
    for (const t of this.tabs.filter((x) => x.set === set)) this.close(t.id, 'evicted');
    this.selected.delete(set);
    this.slotUsed.delete(set);
  }

  /** Moves a displayed tab to `toIndex` among the view's displayed tabs. */
  reorder(id: string, toIndex: number, current: string | null): void {
    const shown = this.displayed(current);
    const from = shown.findIndex((t) => t.id === id);
    if (from < 0) return;
    const tab = shown[from] as PanelTab;
    const rest = shown.filter((t) => t.id !== id);
    const clamped = Math.max(0, Math.min(rest.length, toIndex));
    const before = rest[clamped];
    this.tabs.splice(this.tabs.indexOf(tab), 1);
    const at = before ? this.tabs.indexOf(before) : this.tabs.length;
    this.tabs.splice(at, 0, tab);
  }

  /** The panel X: hides documents and leaves maximised mode; closes nothing. */
  hide(): void {
    this.hidden = true;
    this.maximized = false;
  }

  /** Mod+Shift+I: toggles the documents while any are counted, otherwise the Inspector. */
  toggle(current: string | null): void {
    if (this.counted(current).length > 0) {
      this.hidden = !this.hidden;
      if (this.hidden) this.maximized = false;
    } else {
      this.inspector = !this.inspector;
    }
  }

  setInspector(on: boolean, slot?: SlotId, current: string | null = null): void {
    this.inspector = on;
    if (on && slot) this.select(slotEntry(slot), current);
  }

  setMaximized(on: boolean): void {
    this.maximized = on;
  }

  /**
   * Which tabs stay mounted: the active one, plus up to 3 user-intent keep-mounted tabs per scope,
   * most recently used first. Program tabs are never kept mounted.
   */
  mounted(chatId: string | null, keepMounted: (kind: string) => boolean): PanelTab[] {
    const active = this.activeTab(chatId);
    const keep = this.displayed(chatId)
      .filter((t) => t.intent === 'user' && keepMounted(t.kind) && t !== active)
      .sort((a, b) => b.used - a.used);
    const out: PanelTab[] = active ? [active] : [];
    const perScope = new Map<string, number>();
    for (const t of keep) {
      const scope = t.set === APP_SET ? APP_SET : 'chat';
      const n = perScope.get(scope) ?? 0;
      if (n >= 3) continue;
      perScope.set(scope, n + 1);
      out.push(t);
    }
    return out;
  }
}

export { slotEntry, entrySlot };
