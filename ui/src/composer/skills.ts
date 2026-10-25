// Skill completion: typing "/" and a prefix at the start of the draft offers the
// CLI's own commands and skills; choosing one puts "/name " in the draft.

export interface Skill {
  name: string;
  description: string;
  /** Where the skill comes from, when the host says: shown at the right of its row. */
  scope?: string;
}

export const SCOPE_LABEL: Record<string, string> = { personal: 'Personal skill', project: 'Project skill', plugin: 'Plugin skill', builtin: 'Command' };

/** The typed prefix when the draft is a bare "/word" at its start, else null. */
export function skillQuery(text: string): string | null {
  const m = /^\/([A-Za-z0-9_:-]{0,128})$/.exec(text);
  return m ? m[1]! : null;
}

const norm = (s: string) => s.toLowerCase().replace(/[-_\s]/g, '');

/** Best first: exact, prefix, word prefix, substring, description substring; ties by name. */
export function rankSkills(list: readonly Skill[], query: string, limit = 8): Skill[] {
  const q = query.toLowerCase();
  const nq = norm(query);
  const score = (s: Skill): number => {
    const n = s.name.toLowerCase();
    if (!q) return 4;
    if (n === q || norm(n) === nq) return 0;
    if (n.startsWith(q)) return 1;
    if (n.split(/[-_:\s]/).some((w) => w.startsWith(q))) return 2;
    if (n.includes(q)) return 3;
    if (s.description.toLowerCase().includes(q)) return 5;
    return 99;
  };
  return list
    .map((s) => ({ s, k: score(s) }))
    .filter((x) => x.k < 99)
    .sort((a, b) => a.k - b.k || a.s.name.localeCompare(b.s.name))
    .slice(0, limit)
    .map((x) => x.s);
}

/** The command a draft starts with once arguments follow it ("/review the diff" → "review"). */
export function commandWithArgs(text: string): string | null {
  const m = /^\/([A-Za-z0-9_:-]{1,128})\s/.exec(text);
  return m ? m[1]! : null;
}

export interface SkillMenu {
  /** `pick`: choose a command; `args`: the chosen command stays shown while arguments are typed. */
  mode: 'pick' | 'args';
  items: Skill[];
}

/** What the `/` menu shows for a draft, or null when it is closed. */
export function skillMenu(text: string, list: readonly Skill[], limit = 12): SkillMenu | null {
  const q = skillQuery(text);
  if (q !== null) {
    const items = rankSkills(list, q, limit);
    return items.length ? { mode: 'pick', items } : null;
  }
  const cmd = commandWithArgs(text);
  if (cmd === null) return null;
  const items = list.filter((s) => s.name.toLowerCase() === cmd.toLowerCase()).slice(0, 1);
  return items.length ? { mode: 'args', items } : null;
}

/** Up/Down move the highlight through the menu, wrapping. */
export function moveActive(active: number, count: number, dir: 1 | -1): number {
  if (count <= 0) return -1;
  return (((active + dir) % count) + count) % count;
}
