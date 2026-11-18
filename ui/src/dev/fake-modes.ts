// The dev fake host's modes: a few mode files in the Everyone list and one person's list, each
// chat's own switched-on and switched-off modes, and the ops Settings and the chip row use
// (a plain browser, `npx vite`). Dev builds only.

import { ApiError } from '../api/client.ts';
import type { ModeFile } from '../api/generated/skills/index.ts';
import type { ChatModes } from '../modes/api.ts';
import type { FakeOpHandler } from './fakeHost.ts';

const HOME = 'profile:prof-home';

interface Stored extends Omit<ModeFile, 'revision'> {
  revision: number;
}

function mode(layer: string, name: string, description: string, over: Partial<Stored> = {}): Stored {
  return { name, layer, description, triggers: [], tools: [], mcp: [], skills: [], rules: [], builtins: [], model: null, body: '', revision: 1, ...over };
}

interface ChatState {
  /** Mode name -> what switched it on. */
  on: Map<string, string>;
  off: Set<string>;
}

export function modesFakeOps(): { prefixes: string[]; handler: FakeOpHandler } {
  const files = new Map<string, Stored[]>([
    [
      'everyone',
      [
        mode('everyone', 'writing', 'Drafting and editing prose: where the style notes are and how to review a draft.', {
          triggers: ['draft', 'essay', 'edit'],
          skills: ['release-notes'],
          body: 'Style notes live in the notes folder under style/. Read the last two drafts before starting a new one.',
        }),
        mode('everyone', 'garden', 'The garden plan and the planting calendar.', { triggers: ['plant', 'seeds'], body: 'The beds are numbered in garden/beds.md. Check the calendar before suggesting a sowing date.' }),
      ],
    ],
    [HOME, [mode(HOME, 'errands', 'The weekly errands list and where each shop is.', { body: 'The list is in home/errands.md.' })]],
  ]);
  const chats = new Map<string, ChatState>();

  const need = (key: string): Stored[] => {
    const l = files.get(key);
    if (!l) throw new ApiError('bad_args', 'The layer is `everyone` or `profile:<id>`.');
    return l;
  };
  const out = (m: Stored): ModeFile => ({ ...m, revision: String(m.revision) });
  /** The modes a chat of the home person gets: that person's file over Everyone's of the same name. */
  const merged = (): Stored[] => {
    const byName = new Map<string, Stored>();
    for (const m of [...need('everyone'), ...need(HOME)]) byName.set(m.name, m);
    return [...byName.values()].sort((a, b) => a.name.localeCompare(b.name));
  };
  const named = (names: string[]) => merged().filter((m) => names.includes(m.name));
  const brief = (m: Stored) => ({ name: m.name, description: m.description, layer: m.layer });

  const handler: FakeOpHandler = (op, a, ctx) => {
    const changed = (layer: string) => ctx.emit({ kind: 'knowledge-changed', layer });
    const view = (chatId: string): ChatModes => {
      const chat = (ctx.call('chat_get', { chatId }) as { chat: { projectId: string; started: boolean } }).chat;
      let st = chats.get(chatId);
      // A chat that has started was seeded from its project's defaults when it first launched.
      if (!st && chat.started) {
        st = { on: new Map(), off: new Set() };
        const projects = ctx.call('project_list', { includeArchived: true, includeClosed: true }) as { id: string; defaultModes: string[] }[];
        for (const n of projects.find((p) => p.id === chat.projectId)?.defaultModes ?? []) if (merged().some((m) => m.name === n)) st.on.set(n, 'project');
        chats.set(chatId, st);
      }
      const on = st?.on ?? new Map<string, string>();
      const off = st?.off ?? new Set<string>();
      return {
        available: merged().filter((m) => !on.has(m.name) && !off.has(m.name)).map(brief),
        active: named([...on.keys()]).map((m) => ({ ...brief(m), source: on.get(m.name) ?? 'user' })),
        off: named([...off]).map(brief),
      };
    };
    switch (op) {
      case 'chat_modes_get':
        return view(String(a.chatId));
      case 'chat_modes_set': {
        const chatId = String(a.chatId);
        const known = new Set(merged().map((m) => m.name));
        const names = (k: string) => (Array.isArray(a[k]) ? (a[k] as unknown[]).map(String) : []);
        for (const n of [...names('add'), ...names('remove')]) if (!known.has(n)) throw new ApiError('bad_args', `There is no mode called ${n}.`);
        view(chatId);
        const st = chats.get(chatId) ?? { on: new Map<string, string>(), off: new Set<string>() };
        for (const n of names('add')) {
          st.off.delete(n);
          if (!st.on.has(n)) st.on.set(n, 'user');
        }
        for (const n of names('remove')) {
          st.on.delete(n);
          st.off.add(n);
        }
        chats.set(chatId, st);
        return view(chatId);
      }
      case 'modes_list': {
        if (typeof a.layer !== 'string') return { layer: null, folder: null, modes: merged().map(out) };
        return { layer: a.layer, folder: `C:/Helm/knowledge/${a.layer}/modes`, modes: need(a.layer).map(out) };
      }
      case 'modes_save': {
        const layer = String(a.layer);
        const l = need(layer);
        const name = String(a.name ?? '').trim();
        if (!/^[a-z0-9][a-z0-9-]*$/.test(name)) throw new ApiError('bad_args', 'A mode name is lowercase letters, digits and dashes.');
        const strings = (k: string) => (Array.isArray(a[k]) ? (a[k] as unknown[]).map(String) : []);
        const next = mode(layer, name, String(a.description ?? '').trim(), {
          triggers: strings('triggers'),
          tools: strings('tools'),
          mcp: strings('mcp'),
          skills: strings('skills'),
          rules: strings('rules'),
          builtins: strings('builtins'),
          model: typeof a.model === 'string' && a.model.trim() ? a.model.trim() : null,
          body: String(a.body ?? ''),
        });
        const i = l.findIndex((m) => m.name === name);
        const old = l[i];
        if (a.expectedRevision !== undefined && (!old || String(a.expectedRevision) !== String(old.revision))) throw new ApiError('conflict', 'That mode changed since it was read.');
        const saved = old ? { ...next, revision: old.revision + 1 } : next;
        if (old) l[i] = saved;
        else l.push(saved);
        changed(layer);
        return { mode: out(saved) };
      }
      case 'modes_remove': {
        const layer = String(a.layer);
        const l = need(layer);
        const i = l.findIndex((m) => m.name === a.name);
        if (i < 0) throw new ApiError('bad_args', 'No mode of that name is in the folder.');
        l.splice(i, 1);
        changed(layer);
        return { removed: true };
      }
      default:
        throw new ApiError('unknown_op', op);
    }
  };
  return { prefixes: ['chat_modes_', 'modes_'], handler };
}
