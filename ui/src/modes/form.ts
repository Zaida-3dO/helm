// The mode form in Settings → Skills & rules, as plain functions: a mode file's lists are typed as
// comma separated text and read back into lists.

import type { ModeDraft, ModeFile } from './api.ts';

/** The text boxes of the mode form. */
export interface ModeForm {
  name: string;
  description: string;
  triggers: string;
  mcp: string;
  skills: string;
  rules: string;
  tools: string;
  builtins: string;
  model: string;
  body: string;
}

export const EMPTY_FORM: ModeForm = { name: '', description: '', triggers: '', mcp: '', skills: '', rules: '', tools: '', builtins: '', model: '', body: '' };

/** `a, b ,, a` becomes `['a', 'b']`: trimmed, no blanks, no repeats. */
export function parseList(text: string): string[] {
  const out: string[] = [];
  for (const raw of text.split(',')) {
    const t = raw.trim();
    if (t && !out.includes(t)) out.push(t);
  }
  return out;
}

export function formOf(m: ModeFile): ModeForm {
  return {
    name: m.name,
    description: m.description,
    triggers: m.triggers.join(', '),
    mcp: m.mcp.join(', '),
    skills: m.skills.join(', '),
    rules: m.rules.join(', '),
    tools: m.tools.join(', '),
    builtins: m.builtins.join(', '),
    model: m.model ?? '',
    body: m.body,
  };
}

/** What a save sends for the form (the name travels beside it). */
export function draftOf(f: ModeForm): ModeDraft {
  return {
    description: f.description.trim(),
    triggers: parseList(f.triggers),
    tools: parseList(f.tools),
    mcp: parseList(f.mcp),
    skills: parseList(f.skills),
    rules: parseList(f.rules),
    builtins: parseList(f.builtins),
    model: f.model.trim() || null,
    body: f.body,
  };
}
