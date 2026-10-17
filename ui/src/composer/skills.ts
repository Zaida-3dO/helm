// Skill completion: typing "/" and a prefix at the start of the draft offers the
// CLI's own commands and skills; choosing one puts "/name " in the draft.

export interface Skill {
  name: string;
  description: string;
}

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
