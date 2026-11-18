// The startup set of a project as the Settings form edits it: instructions, context files and the
// MCP servers its chats get, and the modes its new chats start with. The pure parts live here so they can be tested without the form.

import type { ProjectNode, ProjectUpdateArgs } from '../../api/projects.ts';

export const STARTUP_MAX = 4000;
export const STARTUP_LIST_MAX = 20;

/** One path per line; blank lines and repeats are dropped. */
export function parseLines(text: string): string[] {
  const out: string[] = [];
  for (const raw of text.split(/\r?\n/)) {
    const t = raw.trim();
    if (t && !out.includes(t)) out.push(t);
  }
  return out;
}

export interface StartupForm {
  instructions: string;
  files: string;
  servers: string[];
  /** The modes new chats start with (their names). */
  modes: string[];
}

/** The form a project's saved startup set fills in. */
export function formOf(p: Pick<ProjectNode, 'startupInstructions' | 'startupFiles' | 'startupServers' | 'defaultModes'>): StartupForm {
  return { instructions: p.startupInstructions ?? '', files: p.startupFiles.join('\n'), servers: [...p.startupServers], modes: [...p.defaultModes] };
}

/** What a path must look like: absolute, on Windows or elsewhere. */
export function isAbsolutePath(p: string): boolean {
  return /^[A-Za-z]:[\\/]/.test(p) || p.startsWith('/') || p.startsWith('\\\\');
}

/** The first thing wrong with the form, in words, or null. */
export function problem(f: StartupForm): string | null {
  if (f.instructions.trim().length > STARTUP_MAX) return `The instructions are at most ${STARTUP_MAX} characters.`;
  const files = parseLines(f.files);
  if (files.length > STARTUP_LIST_MAX) return `A project lists at most ${STARTUP_LIST_MAX} context files.`;
  const bad = files.find((p) => !isAbsolutePath(p) || p.length > 300);
  if (bad) return `"${bad.slice(0, 60)}" is not an absolute path.`;
  if (f.servers.length > STARTUP_LIST_MAX) return `A project names at most ${STARTUP_LIST_MAX} MCP servers.`;
  if (f.modes.length > STARTUP_LIST_MAX) return `A project starts chats with at most ${STARTUP_LIST_MAX} modes.`;
  return null;
}

/** The update that makes the project match the form: only what changed, or null when nothing did. */
export function changes(p: ProjectNode, f: StartupForm): ProjectUpdateArgs | null {
  const out: ProjectUpdateArgs = { projectId: p.id, revision: p.revision };
  let any = false;
  const text = f.instructions.trim();
  if (text !== (p.startupInstructions ?? '').trim()) {
    out.startupInstructions = text;
    any = true;
  }
  const files = parseLines(f.files);
  if (files.join('\n') !== p.startupFiles.join('\n')) {
    out.startupFiles = files;
    any = true;
  }
  const servers = [...f.servers].sort();
  if (servers.join('\n') !== [...p.startupServers].sort().join('\n')) {
    out.startupServers = f.servers;
    any = true;
  }
  if (f.modes.length !== p.defaultModes.length || !f.modes.every((m) => p.defaultModes.includes(m))) {
    out.defaultModes = f.modes;
    any = true;
  }
  return any ? out : null;
}
