// The tile row's layout, as pure functions. Tile 1 is always New and tile 2 always Search; the
// views (Flagged, Tasks, Schedules) follow in most-recently-used order. When they do not all fit,
// the last slot is a More tile that holds the rest. The saved order lives on this device only.

export const TILE_MIN = 56;
export const TILE_GAP = 4;
/** New, Search and More: the tiles that are always there. */
export const FIXED_SLOTS = 3;

export type ViewTileId = 'flagged' | 'tasks' | 'schedules';

/** The order a fresh install starts with, before any view has been opened. */
export const DEFAULT_VIEW_ORDER: readonly ViewTileId[] = ['flagged', 'tasks', 'schedules'];

/** The `localStorage` key of the saved view order (per device, keeps views that are off). */
export const TILES_KEY = 'helm.sidebar.tiles';

/** How many tiles fit in a row `width` pixels wide (never fewer than New, Search and More). */
export function slotsFit(width: number): number {
  return Math.max(FIXED_SLOTS, Math.floor((width + TILE_GAP) / (TILE_MIN + TILE_GAP)));
}

export interface Split<T> {
  shown: T[];
  more: T[];
}

/** The views drawn as tiles and the views held in More, for a row with `fit` slots. With every
 *  view fitting there is no More. */
export function splitViews<T>(views: readonly T[], fit: number): Split<T> {
  if (2 + views.length <= fit) return { shown: [...views], more: [] };
  const room = Math.max(0, fit - FIXED_SLOTS);
  return { shown: views.slice(0, room), more: views.slice(room) };
}

/** The saved order restricted to the views that exist now, then any view the saved order does not
 *  know yet, in default order. */
export function orderViews<T extends string>(saved: readonly string[], available: readonly T[]): T[] {
  const have = new Set<string>(available);
  const known = saved.filter((id, i) => have.has(id) && saved.indexOf(id) === i) as T[];
  return [...known, ...available.filter((id) => !known.includes(id))];
}

/** A view was opened: it becomes the first view tile. */
export function touchView<T extends string>(order: readonly T[], id: T): T[] {
  return order.includes(id) ? [id, ...order.filter((x) => x !== id)] : [...order];
}

/** One step left (-1) or right (1) among the views. */
export function moveView<T extends string>(order: readonly T[], id: T, dir: -1 | 1): T[] {
  const at = order.indexOf(id);
  const to = at + dir;
  if (at < 0 || to < 0 || to >= order.length) return [...order];
  const next = [...order];
  [next[at], next[to]] = [next[to]!, next[at]!];
  return next;
}

/** A new visible order merged into the saved one: views that are off right now keep their place
 *  after the ones given, so they return where they were. */
export function mergeOrder(saved: readonly string[], next: readonly string[]): string[] {
  return [...next, ...saved.filter((id) => !next.includes(id))];
}

type Store = Pick<Storage, 'getItem' | 'setItem'>;

function defaultStore(): Store | null {
  try {
    return typeof localStorage === 'undefined' ? null : localStorage;
  } catch {
    return null;
  }
}

/** The saved order; a missing or damaged value reads as no order. */
export function readOrder(store: Store | null = defaultStore()): string[] {
  try {
    const parsed: unknown = JSON.parse(store?.getItem(TILES_KEY) ?? '[]');
    return Array.isArray(parsed) ? parsed.filter((x): x is string => typeof x === 'string') : [];
  } catch {
    return [];
  }
}

export function writeOrder(order: readonly string[], store: Store | null = defaultStore()): void {
  try {
    store?.setItem(TILES_KEY, JSON.stringify(order));
  } catch {
    // a full or blocked store: the order lasts until the window closes
  }
}
