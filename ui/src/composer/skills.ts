// Skill completion: typing "/" and a prefix at the start of the draft offers the
// CLI's own commands and skills, and Helm's own commands; choosing one puts "/name " in the draft.
// A Helm command is run by Helm when sent, never passed to Claude.

const norm = (s: string) => s.toLowerCase().replace(/[-_\s]/g, '');

export interface Skill {
  name: string;
  description: string;
  /** Where the skill comes from, when the host says: shown at the right of its row. */
  scope?: string;
}

export const SCOPE_LABEL: Record<string, string> = { personal: 'Personal skill', project: 'Project skill', plugin: 'Plugin skill', builtin: 'Command', helm: 'Helm command', 'helm-skill': 'Helm' };

/** The prefix the CLI may put on a skill's name for each scope that has one. */
const SCOPE_PREFIX: Record<string, string> = { 'helm-skill': 'helm:', personal: 'personal:', project: 'project:' };

/** Whether the user hid this skill. Names match case-insensitively; a Helm, personal or project
 *  skill also matches its bare folder name, the CLI may report it with a prefix. */
export function matchesHidden(s: Skill, hidden: readonly string[]): boolean {
  const n = s.name.toLowerCase();
  const prefix = s.scope ? SCOPE_PREFIX[s.scope] : undefined;
  const bare = prefix && n.startsWith(prefix) ? n.slice(prefix.length) : n;
  return hidden.some((h) => {
    const x = h.trim().toLowerCase();
    return x === n || (bare !== n && x === bare);
  });
}

/** Commands Helm runs itself. They lead the `/` menu's list and shadow a CLI skill of the same name.
 *  Fork, Pin and Archive left the top bar and live here now; Pin and Archive toggle. */
export const HELM_COMMANDS: readonly Skill[] = [
  { name: 'rollover', description: 'Hand this chat’s work to a fresh chat, then archive this one', scope: 'helm' },
  { name: 'fork', description: 'Continue this conversation in a copy of the chat', scope: 'helm' },
  { name: 'pin', description: 'Pin this chat to the top of its board, or unpin it', scope: 'helm' },
  { name: 'archive', description: 'Archive this chat, or restore it if it is archived', scope: 'helm' },
];

/** The menu's list: Helm's commands, then the CLI's (minus any a Helm command shadows). */
export function withHelmCommands(list: readonly Skill[]): Skill[] {
  const own = new Set(HELM_COMMANDS.map((c) => norm(c.name)));
  return [...HELM_COMMANDS, ...list.filter((s) => !own.has(norm(s.name)))];
}

/** The Helm command a whole draft names, however it is spaced or hyphenated ("/rollover",
 *  "/roll-over", "/Roll over"), or null. Sending such a draft runs the command. */
export function helmCommand(text: string): Skill | null {
  const m = /^\/([A-Za-z][A-Za-z _-]{0,40})$/.exec(text.trim());
  if (!m) return null;
  const n = norm(m[1]!);
  return HELM_COMMANDS.find((c) => norm(c.name) === n) ?? null;
}

/** The typed prefix when the draft is a bare "/word" at its start, else null. */
export function skillQuery(text: string): string | null {
  const m = /^\/([A-Za-z0-9_:-]{0,128})$/.exec(text);
  return m ? m[1]! : null;
}

/** The "/word" at the caret: the range it covers and the query typed so far. */
export interface SlashWord {
  start: number;
  end: number;
  query: string;
}

/**
 * A "/word" being typed at the caret, wherever the caret sits in the draft (a `/` after a space
 * opens the menu mid-sentence): the range it covers, including a word the caret sits inside, and
 * the query typed so far. null when the caret is not on one.
 */
export function slashAt(text: string, caret: number): SlashWord | null {
  if (caret < 0 || caret > text.length) return null;
  const m = /(?:^|\s)\/([A-Za-z0-9_:-]{0,128})$/.exec(text.slice(0, caret));
  if (!m) return null;
  const query = m[1]!;
  const rest = /^\S*/.exec(text.slice(caret))![0];
  return { start: caret - query.length - 1, end: caret + rest.length, query };
}

/** What choosing a skill at such a word writes: the word replaced by "/name " — the separating
 *  space added only when the text after the word does not already start with one. */
export function applySlash(text: string, at: { start: number; end: number }, name: string): { text: string; caret: number } {
  const ins = `/${name}`;
  const space = text[at.end] === undefined || !/\s/.test(text[at.end]!) ? ' ' : '';
  return { text: text.slice(0, at.start) + ins + space + text.slice(at.end), caret: at.start + ins.length + space.length };
}


/** Best first: exact, prefix, word prefix, substring, description substring; ties by name. */
export function rankSkills(list: readonly Skill[], query: string, limit = 8): Skill[] {
  const q = query.toLowerCase();
  const nq = norm(query);
  const score = (s: Skill): number => {
    const n = s.name.toLowerCase();
    // Nothing typed yet: Helm's own commands lead the menu, the rest by name.
    if (!q) return s.scope === 'helm' ? 3 : 4;
    if (n === q || norm(n) === nq) return 0;
    if (n.startsWith(q) || (nq && norm(n).startsWith(nq))) return 1;
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
  // A Helm command typed with a space ("/roll over") is still that command.
  const spaced = /^\/[A-Za-z]+[ ]+[A-Za-z]*$/.exec(text) ? norm(text.slice(1)) : '';
  const own = spaced ? list.filter((s) => s.scope === 'helm' && norm(s.name).startsWith(spaced)) : [];
  if (own.length) return { mode: 'pick', items: own };
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
