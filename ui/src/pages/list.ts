// One Pages list, as plain data: this PC's pins first, then every paired PC's, flat — the PC is a
// detail on a row, never a group header. Pins are de-duplicated by (PC, path), and a PC whose
// hello named its profiles only contributes pins of a profile acting for the person on this PC
// (pins made before profiles existed, with no profile, stay; a PC that says nothing about its
// profiles keeps its pins).

import type { PinnedPage, RemotePages } from './api.ts';
import type { DevicesList } from '../devices/api.ts';

/** The letter tile for a page with no icon of its own: the title's first character, upper case. */
export function tileLetter(title: string): string {
  const first = Array.from(title.trim())[0];
  return first ? first.toUpperCase() : '';
}

export interface PageRow {
  page: PinnedPage;
  /** The PC that holds the pin; null for this PC's own. */
  device: { uid: string; name: string } | null;
}

/** A PC whose pins could not be read, in words. */
export interface PageProblem {
  uid: string;
  name: string;
  message: string;
}

/** The merged list, and one line per PC that could not be read. `personId` is the hub person of
 *  the acting profile; null (no active profile, or an unlinked one) keeps every pin. */
export function mergePages(
  local: readonly PinnedPage[],
  remote: readonly RemotePages[],
  list: DevicesList | null,
  personId: string | null,
): { rows: PageRow[]; problems: PageProblem[] } {
  const rows: PageRow[] = local.map((p) => ({ page: p, device: null }));
  const problems: PageProblem[] = [];
  const ordered = [...remote].sort((a, b) => a.name.localeCompare(b.name, undefined, { sensitivity: 'base' }));
  for (const d of ordered) {
    if (d.problem) {
      problems.push({ uid: d.deviceUid, name: d.name, message: d.problem.message });
      continue;
    }
    if (d.pages.length === 0) continue;
    let allowed: Set<string> | null = null;
    if (personId !== null) {
      const device = list?.devices.find((x) => x.uid === d.deviceUid) ?? null;
      if (device?.profiles) allowed = new Set(device.profiles.filter((p) => p.personId === personId).map((p) => p.profileId));
    }
    const seen = new Set<string>();
    for (const p of d.pages) {
      if (allowed && p.profileId !== '' && !allowed.has(p.profileId)) continue;
      const key = `${d.deviceUid}\u0000${p.path}`;
      if (seen.has(key)) continue;
      seen.add(key);
      rows.push({ page: p, device: { uid: d.deviceUid, name: d.name } });
    }
  }
  return { rows, problems };
}
