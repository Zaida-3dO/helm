// The dev fake host's memory-index ops (Settings → Memory in a plain browser): a made-up set of
// sources with plausible counts and status lines. It keeps them in memory and answers with the
// whole state, like the real host; nothing reaches any hub. Dev builds only.

import { ApiError } from '../api/client.ts';
import type { MemindexKind, MemindexScope, MemindexSource, MemindexState } from '../settings/memory/model.ts';
import type { FakeOpHandler } from './fakeHost.ts';

interface FakeSource extends MemindexSource {
  createdAt: number;
}

const here = (p: string) => /^([a-zA-Z]:[\\/]|\\\\)/.test(p);

export function memindexFakeOps(): { prefixes: string[]; handler: FakeOpHandler } {
  const settings = { enabled: false, serverId: 'ollama', model: 'nomic-embed-text', nightly: '03:30' };
  let hub: 'ready' | 'down' | 'too_old' = 'ready';
  const sources: FakeSource[] = [
    {
      id: 'chats',
      kind: 'helm_chats',
      path: '',
      personId: '',
      scope: 'me',
      include: [],
      exclude: [],
      enabled: true,
      docs: 214,
      pushed: 214,
      status: 'Up to date.',
      lastScan: Date.now() - 40 * 60_000,
      createdAt: Date.now() - 86_400_000,
    },
    {
      id: 'a1b2c3d4',
      kind: 'folder',
      path: 'C:\\Users\\dev\\Notes',
      personId: '',
      scope: 'everyone',
      include: [],
      exclude: ['Private/**'],
      enabled: true,
      docs: 96,
      pushed: 95,
      status: 'Watching for changes.',
      lastScan: Date.now() - 90_000,
      createdAt: Date.now() - 3 * 86_400_000,
    },
  ];
  let seq = 1;
  let pending = 12;

  const view = (): MemindexState => ({
    ...settings,
    hub,
    status: !settings.enabled ? 'Memory indexing is off.' : hub === 'too_old' ? 'The hub needs updating for search.' : hub === 'down' ? 'Waiting for the hub.' : 'Up to date.',
    pending,
    sources: sources.map(({ createdAt: _createdAt, ...s }) => s),
  });

  const handler: FakeOpHandler = (op, a, ctx) => {
    const changed = () => ctx.emit({ kind: 'memindex' });
    switch (op) {
      case 'memindex_state':
        return view();
      case 'memindex_settings_save': {
        if (typeof a.enabled === 'boolean') settings.enabled = a.enabled;
        if (typeof a.serverId === 'string') settings.serverId = a.serverId.trim();
        if (typeof a.model === 'string') {
          const m = a.model.trim();
          if (m && (m.length > 100 || !/^[A-Za-z0-9._:/-]+$/.test(m))) throw new ApiError('refused', 'The model name may hold letters, digits and . - _ : / only.');
          settings.model = m || 'nomic-embed-text';
        }
        if (typeof a.nightly === 'string') {
          if (!/^([01]\d|2[0-3]):[0-5]\d$/.test(a.nightly.trim())) throw new ApiError('refused', 'The nightly scan time is HH:MM, like 03:30.');
          settings.nightly = a.nightly.trim();
        }
        changed();
        return view();
      }
      case 'memindex_sources_save': {
        const old = typeof a.id === 'string' && a.id ? sources.find((s) => s.id === a.id) : undefined;
        if (a.id && !old) throw new ApiError('refused', 'That source is not registered.');
        const kind = (old?.kind ?? a.kind) as MemindexKind | undefined;
        if (!kind) throw new ApiError('refused', 'Say what kind of source this is.');
        const single = kind === 'helm_chats' || kind === 'claude_code';
        if (!old && single && sources.some((s) => s.kind === kind)) throw new ApiError('refused', `${kind} is already a source.`);
        const hasPath = kind === 'folder' || kind === 'chat_logs';
        const path = hasPath ? (typeof a.path === 'string' ? a.path.trim() : old?.path ?? '') : '';
        if (hasPath) {
          if (!path) throw new ApiError('refused', 'Choose a folder.');
          if (!here(path)) throw new ApiError('refused', 'The folder must be a full path.');
          if (sources.some((s) => s.kind === kind && s.path === path && s !== old)) throw new ApiError('refused', 'That folder is already a source.');
        }
        const globs = (v: unknown, keep: string[]): string[] => {
          if (!Array.isArray(v)) return keep;
          const list = v.map((g) => String(g).trim()).filter(Boolean);
          if (list.length > 50 || list.some((g) => g.length > 200)) throw new ApiError('refused', 'Too many patterns, or one is too long.');
          return list;
        };
        const src: FakeSource = {
          id: old?.id ?? (single ? kind : `f${(seq += 1).toString(16)}x${Math.floor(Math.random() * 0xffff).toString(16)}`),
          kind,
          path,
          personId: typeof a.personId === 'string' ? a.personId.trim() : old?.personId ?? '',
          scope: (a.scope as MemindexScope | undefined) ?? old?.scope ?? 'me',
          include: globs(a.include, old?.include ?? []),
          exclude: globs(a.exclude, old?.exclude ?? []),
          enabled: typeof a.enabled === 'boolean' ? a.enabled : old?.enabled ?? true,
          docs: old?.docs ?? 0,
          pushed: old?.pushed ?? 0,
          status: old?.status ?? 'Waiting for the first scan.',
          lastScan: old?.lastScan ?? 0,
          createdAt: old?.createdAt ?? Date.now(),
        };
        if (old) Object.assign(old, src);
        else sources.push(src);
        changed();
        return view();
      }
      case 'memindex_sources_remove': {
        const i = sources.findIndex((s) => s.id === a.id);
        if (i < 0) throw new ApiError('refused', 'That source is not registered.');
        sources.splice(i, 1);
        changed();
        return view();
      }
      case 'memindex_reindex': {
        const now = Date.now();
        for (const s of sources) {
          if (typeof a.sourceId === 'string' && a.sourceId && s.id !== a.sourceId) continue;
          s.lastScan = now;
          if (s.status === 'Waiting for the first scan.') s.status = settings.enabled ? 'Up to date.' : 'Switched off.';
        }
        pending = 0;
        changed();
        return { started: true };
      }
      default:
        throw new ApiError('unknown_op', op);
    }
  }

  /** Lets a dev URL show the states the real hub can report: `?memhub=down|too_old`. */
  const param = typeof location === 'undefined' ? null : new URLSearchParams(location.search).get('memhub');
  if (param === 'down' || param === 'too_old') hub = param;

  return { prefixes: ['memindex_'], handler };
}
