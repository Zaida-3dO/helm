// Recently opened paths for quick-open: at most 10, newest first, kept in the renderer's own
// storage (a convenience list, never sent anywhere).

const KEY = 'helm.quickOpen.recent';
export const MAX_RECENT = 10;

let memory: string[] | null = null;

function storage(): Storage | null {
  try {
    return typeof localStorage === 'undefined' ? null : localStorage;
  } catch {
    return null;
  }
}

export function recentPaths(): string[] {
  if (memory) return memory;
  try {
    const raw = storage()?.getItem(KEY);
    const v: unknown = raw ? JSON.parse(raw) : [];
    memory = Array.isArray(v) ? v.filter((x): x is string => typeof x === 'string').slice(0, MAX_RECENT) : [];
  } catch {
    memory = [];
  }
  return memory;
}

export function addRecent(path: string): void {
  const key = path.toLowerCase();
  memory = [path, ...recentPaths().filter((p) => p.toLowerCase() !== key)].slice(0, MAX_RECENT);
  try {
    storage()?.setItem(KEY, JSON.stringify(memory));
  } catch {
    // Storage full or unavailable: the in-memory list still works for this session.
  }
}
