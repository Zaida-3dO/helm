// The dev fake host's local model ops (Settings → Local models and the model picker in a plain
// browser). Like the real host it asks no server until told to: `local` in the catalogue stays
// empty until a refresh, and the keys are kept only as flags. Nothing here makes a network call.
// Dev builds only.

import { ApiError } from '../api/client.ts';
import type { LocalModelInfo } from '../api/types.ts';
import type { LocalModelsState, ServerView } from '../settings/localmodels/model.ts';
import type { FakeOpHandler } from './fakeHost.ts';

type Server = Omit<ServerView, 'reachable' | 'note'>;

/** What each made-up server lists, and whether it answers. The "office" one never does. */
const SHELF: Record<string, { answers: boolean; models: Omit<LocalModelInfo, 'server' | 'kind' | 'reachable' | 'startable'>[] }> = {
  strata: {
    answers: true,
    models: [
      { id: 'local/strata/qwen3.8-flash', name: 'Qwen 3.8 Flash', contextTokens: 262144, loaded: true },
      { id: 'local/strata/qwen3.8-coder', name: 'Qwen 3.8 Coder', contextTokens: 131072, loaded: false },
    ],
  },
  ollama: { answers: true, models: [{ id: 'local/ollama/llama3.2:3b', name: 'Llama 3.2 3B', contextTokens: 131072, sizeBytes: 2_000_000_000, loaded: false }] },
  office: { answers: false, models: [{ id: 'local/office/qwen3:30b-a3b', name: 'Qwen3 30B A3B', contextTokens: 40960 }] },
};

const here = (url: string) => /^http:\/\/(127\.0\.0\.1|localhost|\[::1\])(:\d+)?(\/|$)/.test(url.trim());

/** The local models as the catalogue holds them: nothing until a refresh has asked the servers. */
export interface LocalCatalogue {
  local(): LocalModelInfo[];
}

export function localModelsFakeOps(): { prefixes: string[]; handler: FakeOpHandler; catalogue: LocalCatalogue } {
  const servers: Server[] = [
    { id: 'strata', name: 'Strata', kind: 'strata', baseUrl: 'http://127.0.0.1:8081', enabled: true, model: 'qwen3.8-flash', autoStart: true, hasKey: false },
    { id: 'ollama', name: 'Ollama', kind: 'ollama', baseUrl: 'http://127.0.0.1:8082', enabled: true, autoStart: true, hasKey: false },
    { id: 'office', name: 'Ollama (office)', kind: 'ollama', baseUrl: 'http://127.0.0.1:8083', enabled: true, autoStart: true, hasKey: false },
  ];
  const settings = { ollamaContext: 65536, keepAliveMinutes: 15, strataNeedGb: 12, tools: 'lean' as const, compareWith: 'sonnet' as 'opus' | 'sonnet' | 'haiku' };
  const ranges = {
    ollamaContextRange: { min: 8192, max: 262144, default: 65536 },
    keepAliveRange: { min: 1, max: 240, default: 15 },
    strataNeedRange: { min: 4, max: 48, default: 12 },
  };
  /** Which servers have been asked, and what they said; empty until a refresh or a test. */
  const asked = new Map<string, boolean>();
  let cached: LocalModelInfo[] = [];

  const view = (): LocalModelsState => ({
    servers: servers.map((s) => {
      const answered = asked.get(s.id);
      return { ...s, ...(answered === undefined ? {} : { reachable: answered }), ...(answered === false ? { note: 'Nothing answered at that address. Check that the server is running.' } : {}) };
    }),
    ...settings,
    ...ranges,
  });

  const rows = (): LocalModelInfo[] =>
    servers
      .filter((s) => s.enabled)
      .flatMap((s) =>
        (SHELF[s.id]?.models ?? []).map((m) => ({
          ...m,
          server: s.name,
          kind: s.kind,
          reachable: SHELF[s.id]?.answers === true,
          // A Strata set to be started keeps its rows choosable while it is down.
          startable: s.kind === 'strata' && s.autoStart && SHELF[s.id]?.answers !== true,
        })),
      );

  const handler: FakeOpHandler = (op, a, ctx) => {
    const changed = () => ctx.emit({ kind: 'local_models' });
    switch (op) {
      case 'local_models_state':
        return view();
      case 'local_models_refresh':
        for (const s of servers) if (s.enabled) asked.set(s.id, SHELF[s.id]?.answers === true);
        cached = rows();
        return { models: cached, notes: [] };
      case 'local_models_save': {
        if (Array.isArray(a.servers)) {
          const next: Server[] = [];
          for (const x of a.servers as Partial<Server>[]) {
            const old = servers.find((s) => s.id === x.id);
            if (old && x.kind && x.kind !== old.kind) throw new ApiError('refused', `${old.name} is ${old.kind}; a server's kind cannot be changed. Remove it and add a new one.`);
            if (!String(x.name ?? '').trim()) throw new ApiError('refused', 'Every server needs a name.');
            if (!here(String(x.baseUrl ?? ''))) throw new ApiError('refused', 'A local model server must be on this machine and use plain http (127.0.0.1, localhost or ::1).');
            const id = old?.id ?? String(x.name).toLowerCase().replace(/[^a-z0-9]+/g, '-').replace(/^-|-$/g, '') + (servers.some((s) => s.id === String(x.name).toLowerCase()) ? '-2' : '');
            next.push({
              id,
              name: String(x.name).trim(),
              kind: x.kind ?? 'ollama',
              baseUrl: String(x.baseUrl).trim(),
              enabled: x.enabled !== false,
              autoStart: x.autoStart !== false,
              hasKey: old?.hasKey ?? false,
              ...(x.model ? { model: x.model } : {}),
              ...('folder' in x && x.folder ? { folder: x.folder } : {}),
              ...('preset' in x && x.preset ? { preset: x.preset } : {}),
            });
          }
          servers.splice(0, servers.length, ...next);
          for (const id of [...asked.keys()]) if (!servers.some((s) => s.id === id)) asked.delete(id);
        }
        const num = (v: unknown, r: { min: number; max: number }, what: string) => {
          if (typeof v !== 'number' || v < r.min || v > r.max) throw new ApiError('refused', `${what} must be between ${r.min} and ${r.max}.`);
          return v;
        };
        if (a.ollamaContext !== undefined) settings.ollamaContext = num(a.ollamaContext, ranges.ollamaContextRange, 'The context size');
        if (a.keepAliveMinutes !== undefined) settings.keepAliveMinutes = num(a.keepAliveMinutes, ranges.keepAliveRange, 'The keep-alive time');
        if (a.strataNeedGb !== undefined) settings.strataNeedGb = num(a.strataNeedGb, ranges.strataNeedRange, 'The memory figure');
        if (a.tools === 'lean' || a.tools === 'full') settings.tools = a.tools as 'lean';
        if (a.compareWith === 'opus' || a.compareWith === 'sonnet' || a.compareWith === 'haiku') settings.compareWith = a.compareWith;
        changed();
        return view();
      }
      case 'local_server_key_set': {
        const s = servers.find((x) => x.id === a.serverId);
        if (!s) throw new ApiError('not_found', 'No such server.');
        if (!String(a.key ?? '').trim()) throw new ApiError('refused', 'Paste the key first.');
        s.hasKey = true;
        changed();
        return view();
      }
      case 'local_server_key_forget': {
        const s = servers.find((x) => x.id === a.serverId);
        if (!s) throw new ApiError('not_found', 'No such server.');
        s.hasKey = false;
        changed();
        return view();
      }
      case 'local_server_test': {
        const s = servers.find((x) => x.id === a.serverId);
        if (!s) throw new ApiError('not_found', 'No such server.');
        const shelf = SHELF[s.id];
        asked.set(s.id, shelf?.answers === true);
        changed();
        return shelf?.answers
          ? { ok: true, message: `${s.name} answered and lists ${shelf.models.length} model${shelf.models.length === 1 ? '' : 's'}.`, models: shelf.models.length }
          : { ok: false, message: `Nothing answered at ${s.baseUrl}. Check that ${s.name} is running.` };
      }
      default:
        throw new ApiError('unknown_op', op);
    }
  };
  return { prefixes: ['local_models_', 'local_server_'], handler, catalogue: { local: () => cached } };
}
