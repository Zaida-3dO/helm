// Settings → MCP logic with no React in it: per-server readiness, the empty
// page's readiness, preset drafts and the editor-to-save conversion.

import type { McpPreset, McpServer, ServerStatus, ValueRef } from '../../api/mcp.ts';

export interface Readiness {
  state: 'off' | 'ready' | 'inactive';
  reason: string;
  actionLabel?: string;
  actionTarget?: string;
}

function tokenMissing(s: McpServer, presets: McpPreset[]): boolean {
  const p = presets.find((x) => x.id === s.preset);
  if (!p || s.transport.type === 'stdio') return false;
  const want = p.tokenHeader.toLowerCase();
  const h = Object.entries(s.transport.headers ?? {}).find(([k]) => k.toLowerCase() === want)?.[1];
  return !h || (h.kind === 'literal' && !h.secretId && !h.value);
}

export function serverReadiness(s: McpServer, status: ServerStatus | undefined, presets: McpPreset[] = []): Readiness {
  if (!s.enabled) return { state: 'off', reason: 'Turned off: chats do not use this server.', actionLabel: 'Turn on', actionTarget: s.id };
  if (status?.state === 'native') {
    return { state: 'inactive', reason: 'Claude Code already loads this server natively, so Helm leaves it to Claude Code.' };
  }
  if (tokenMissing(s, presets)) return { state: 'inactive', reason: 'Needs its token.', actionLabel: 'Add token', actionTarget: s.id };
  switch (status?.state) {
    case 'failed':
      return { state: 'inactive', reason: status.error ?? 'The last connection failed.', actionLabel: 'Edit', actionTarget: s.id };
    case 'signin_required':
      return { state: 'inactive', reason: 'Needs you to sign in.', actionLabel: 'Sign in', actionTarget: s.id };
    case 'connected':
      return { state: 'ready', reason: status.toolCount !== undefined ? `Connected · ${status.toolCount} tools` : 'Connected' };
    case 'connecting':
      return { state: 'ready', reason: 'Connecting…' };
    default:
      return { state: 'ready', reason: 'Ready: connects when a chat first uses it.' };
  }
}

/** The page-level notice: only an empty list has one, and it says why and where to act. */
export function pageReadiness(servers: McpServer[]): Readiness | null {
  if (servers.length > 0) return null;
  return {
    state: 'off',
    reason: 'No MCP servers yet, so chats only have Claude Code’s own servers. Add Agent Standup, Home Assistant or the Helm hub from a preset, or add your own.',
    actionLabel: 'Add a server',
    actionTarget: 'presets',
  };
}

export function draftFromPreset(p: McpPreset, taken: string[]): McpServer {
  let id = p.id;
  for (let i = 2; taken.includes(id); i++) id = `${p.id.slice(0, 21)}_${i}`;
  return {
    id,
    name: p.name,
    enabled: true,
    preset: p.id,
    allowPrivateHttp: false,
    transport: {
      type: p.transport,
      url: '',
      headers: { [p.tokenHeader]: { kind: 'literal', value: '', prefix: p.tokenPrefix } },
    },
  };
}

export function blankServer(taken: string[]): McpServer {
  let id = 'server';
  for (let i = 2; taken.includes(id); i++) id = `server_${i}`;
  return { id, name: 'New server', enabled: true, allowPrivateHttp: false, transport: { type: 'stdio', command: '' } };
}

type Refs = { [key in string]?: ValueRef };

function cleanRefs(m: Refs | undefined): Refs | undefined {
  if (!m) return undefined;
  const out: Refs = {};
  for (const [k, r] of Object.entries(m)) {
    const name = k.trim();
    if (!name || !r) continue;
    if (r.kind === 'literal') {
      if (r.value) out[name] = { kind: 'literal', value: r.value, ...(r.prefix ? { prefix: r.prefix } : {}) };
      else if (r.secretId) out[name] = { kind: 'literal', secretId: r.secretId, ...(r.prefix ? { prefix: r.prefix } : {}) };
      // An empty new literal is left out rather than saved empty.
    } else if (r.value) {
      out[name] = { kind: r.kind, value: r.value, ...(r.prefix ? { prefix: r.prefix } : {}) };
    }
  }
  return Object.keys(out).length ? out : undefined;
}

/** The editor's state as the save op wants it: only the chosen transport's fields, blank entries
 * dropped, a kept secret sent back by id. */
export function toSaveable(d: McpServer): McpServer {
  const t = d.transport;
  const transport: McpServer['transport'] =
    t.type === 'stdio'
      ? {
          type: 'stdio',
          command: (t.command ?? '').trim(),
          ...(t.args?.length ? { args: t.args } : {}),
          ...(cleanRefs(t.env) ? { env: cleanRefs(t.env) } : {}),
        }
      : {
          type: t.type,
          url: (t.url ?? '').trim(),
          ...(cleanRefs(t.headers) ? { headers: cleanRefs(t.headers) } : {}),
          ...(t.oauth ? { oauth: t.oauth } : {}),
        };
  return {
    id: d.id,
    name: d.name.trim(),
    enabled: d.enabled,
    allowPrivateHttp: t.type === 'stdio' ? false : d.allowPrivateHttp,
    ...(d.preset ? { preset: d.preset } : {}),
    transport,
  };
}

/** "http://" to a host other than this computer: the card shows the unencrypted warning. */
export function isPlainRemoteHttp(url: string | undefined): boolean {
  if (!url) return false;
  try {
    const u = new URL(url);
    const h = u.hostname.replace(/^\[|\]$/g, '').toLowerCase();
    return u.protocol === 'http:' && h !== 'localhost' && h !== '::1' && !h.startsWith('127.');
  } catch {
    return false;
  }
}
