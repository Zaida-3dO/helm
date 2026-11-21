// Settings → MCP logic with no React in it: per-server readiness, the empty
// page's readiness, preset drafts and the editor-to-save conversion.

import type { ChatMcpProblem, McpPreset, McpServer, ServerStatus, ValueRef } from '../../api/mcp.ts';
import type { OAuthConfig } from '../../api/generated/mcp/index.ts';
import { GOOGLE_PRESET, keepsBridge } from './google.ts';
import type { OAuthStatus } from './oauth.ts';

export interface Readiness {
  state: 'off' | 'ready' | 'inactive';
  reason: string;
  actionLabel?: string;
  actionTarget?: string;
}

/** The built-in browser server (the broker on this PC). Its settings are the user's alone. */
export const BROKER_PRESET = 'browser_broker';

export function isBrokerServer(s: McpServer): boolean {
  return s.preset === BROKER_PRESET;
}

function tokenMissing(s: McpServer, presets: McpPreset[]): boolean {
  const p = presets.find((x) => x.id === s.preset);
  if (!p || s.transport.type === 'stdio') return false;
  const want = p.tokenHeader.toLowerCase();
  const h = Object.entries(s.transport.headers ?? {}).find(([k]) => k.toLowerCase() === want)?.[1];
  return !h || (h.kind === 'literal' && !h.secretId && !h.value);
}

/** A program preset names environment values the user types in (a sign-in client's id and secret). */
function envMissing(s: McpServer, presets: McpPreset[]): boolean {
  const p = presets.find((x) => x.id === s.preset);
  if (!p?.secretEnv?.length || s.transport.type !== 'stdio') return false;
  // A Google accounts server keeps its sign-in client in its own dialog, not in the environment.
  if (p.id === GOOGLE_PRESET) return false;
  const env = s.transport.env ?? {};
  return p.secretEnv.some((k) => {
    const r = env[k];
    if (!r) return true;
    return r.kind === 'literal' ? !r.secretId && !r.value : !r.value;
  });
}

export function serverReadiness(s: McpServer, status: ServerStatus | undefined, presets: McpPreset[] = [], oauth?: OAuthStatus): Readiness {
  const browserSignIn = s.transport.type !== 'stdio' && !!s.transport.oauth;
  if (!s.enabled) return { state: 'off', reason: 'Turned off: chats do not use this server.', actionLabel: 'Turn on', actionTarget: s.id };
  // A Google accounts server without a client of its own is served by Helm's own Gmail tools:
  // no process starts, no connection is made, and the row says what answers for it.
  if (s.preset === GOOGLE_PRESET && !keepsBridge(s)) {
    return { state: 'ready', reason: 'Built in: served by Helm.' };
  }
  if (status?.state === 'native') {
    return { state: 'inactive', reason: 'Claude Code already loads this server natively, so Helm leaves it to Claude Code.' };
  }
  // A browser-signed-in server needs you, not a token field: say so before the token rules.
  if (browserSignIn && !oauth?.connected && status?.state !== 'connecting') {
    return { state: 'inactive', reason: 'Needs you to connect in your browser.', actionLabel: 'Connect', actionTarget: s.id };
  }
  if (tokenMissing(s, presets)) return { state: 'inactive', reason: 'Needs its token.', actionLabel: 'Add token', actionTarget: s.id };
  if (envMissing(s, presets))
    return {
      state: 'inactive',
      reason: s.preset === BROKER_PRESET ? 'Needs its broker token.' : 'Needs its client id and secret.',
      actionLabel: 'Edit',
      actionTarget: s.id,
    };
  switch (status?.state) {
    case 'failed':
      return { state: 'inactive', reason: status.error ?? 'The last connection failed.', actionLabel: 'Edit', actionTarget: s.id };
    case 'signin_required':
      return browserSignIn
        ? { state: 'inactive', reason: 'Needs you to sign in again.', actionLabel: 'Sign in again', actionTarget: s.id }
        : { state: 'inactive', reason: 'Needs you to sign in.', actionLabel: 'Sign in', actionTarget: s.id };
    case 'connected':
      return { state: 'ready', reason: status.toolCount !== undefined ? `Connected · ${status.toolCount} tools` : 'Connected' };
    case 'connecting':
      return { state: 'ready', reason: 'Connecting…' };
    default:
      return { state: 'ready', reason: 'Ready: connects when a chat first uses it.' };
  }
}

/** The chat view's one line: a server this chat was given that it cannot use on this PC. The
 * reason is the host's own words (its fallback text when the worker sent none); the line says
 * where to fix it. One row whatever the count: the first name carries it, "and N more" follows. */
export function chatMcpNoticeText(problems: ChatMcpProblem[]): string | null {
  const first = problems[0];
  if (!first) return null;
  const rest = problems.slice(1);
  const who = rest.length ? `${first.name} and ${rest.length} more` : first.name;
  const reason = first.reason.trim();
  const period = /[.!?…]$/.test(reason) ? '' : '.';
  return `${who} ${rest.length ? 'are' : 'is'} not connected on this PC: ${reason}${period} Fix it in Settings → MCP.`;
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
  if (p.transport === 'stdio') {
    // A program preset: what to run, and an empty row for each variable it expects (a secret
    // one for each name whose value must be typed in). A Google accounts server types none:
    // its client is set in the accounts dialog.
    const names = [...new Set([...(p.secretEnv ?? []), ...(p.draftEnv ?? [])])];
    const env =
      p.id === GOOGLE_PRESET
        ? {}
        : Object.fromEntries(names.map((k) => [k, { kind: 'literal' as const, value: '' }]));
    return {
      id,
      name: p.name,
      enabled: true,
      preset: p.id,
      allowPrivateHttp: false,
      transport: {
        type: 'stdio',
        command: p.command ?? '',
        args: [...(p.args ?? [])],
        env,
      },
    };
  }
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

/** A secret's values for single profiles as a save wants them: a typed value replaces, a kept
 * one rides back as its id, an empty entry is dropped. */
function cleanProfileSecrets(m: ValueRef['profiles']): ValueRef['profiles'] {
  if (!m) return undefined;
  const out: NonNullable<ValueRef['profiles']> = {};
  for (const [id, p] of Object.entries(m)) {
    if (!p) continue;
    if (p.value) out[id] = { value: p.value };
    else if (p.secretId) out[id] = { secretId: p.secretId };
  }
  return Object.keys(out).length ? out : undefined;
}

/** A secret's own value for one profile: typed (`value` a string; an empty one keeps what is
 * saved) or taken away (`null`). The shared value is never touched. */
export function withProfileValue(r: ValueRef, profileId: string, value: string | null): ValueRef {
  const profiles = { ...(r.profiles ?? {}) };
  if (value === null) delete profiles[profileId];
  else profiles[profileId] = { ...(profiles[profileId] ?? {}), value };
  const { profiles: _drop, ...rest } = r;
  return Object.keys(profiles).length ? { ...rest, profiles } : rest;
}

function cleanRefs(m: Refs | undefined): Refs | undefined {
  if (!m) return undefined;
  const out: Refs = {};
  for (const [k, r] of Object.entries(m)) {
    const name = k.trim();
    if (!name || !r) continue;
    if (r.kind === 'literal') {
      const profiles = cleanProfileSecrets(r.profiles);
      const own = profiles ? { profiles } : {};
      const prefix = r.prefix ? { prefix: r.prefix } : {};
      if (r.value) out[name] = { kind: 'literal', value: r.value, ...prefix, ...own };
      else if (r.secretId) out[name] = { kind: 'literal', secretId: r.secretId, ...prefix, ...own };
      // A secret with only profile values has no shared one: a chat of any other profile finds
      // it empty and the row says it needs its token.
      else if (profiles) out[name] = { kind: 'literal', ...prefix, ...own };
      // An empty new literal is left out rather than saved empty.
    } else if (r.kind === 'chatPerson') {
      // Filled by the host per chat, so it never has a value of its own.
      out[name] = { kind: 'chatPerson' };
    } else if (r.value) {
      out[name] = { kind: r.kind, value: r.value, ...(r.prefix ? { prefix: r.prefix } : {}) };
    }
  }
  return Object.keys(out).length ? out : undefined;
}

/** The browser sign-in part of a save: an empty object means "sign in through the browser,
 * register automatically"; a filled-in client is kept, an empty new secret is dropped, a kept
 * one rides back as its id. */
function cleanOauth(o: OAuthConfig): OAuthConfig {
  const out: OAuthConfig = {};
  for (const k of ['clientId', 'scope', 'redirectUri'] as const) {
    const v = o[k];
    if (v && v.trim()) out[k] = v.trim();
  }
  const s = o.clientSecret;
  if (s?.kind === 'literal') {
    // A typed value (even over a saved one) replaces; blanking the box keeps the saved secret.
    if (s.value && s.value.trim()) out.clientSecret = { kind: 'literal', value: s.value };
    else if (s.secretId) out.clientSecret = { kind: 'literal', secretId: s.secretId };
  } else if (s?.value) {
    out.clientSecret = s;
  }
  return out;
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
          ...(t.oauth ? { oauth: cleanOauth(t.oauth) } : {}),
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

/** One reference in a copied server: an environment name and the chat's person cross as they
 * stand; anything else (a literal value, a saved id, another PC's file path) crosses as an
 * empty secret slot. Applied again on arrival, so nothing secret can be saved from the dialog
 * even if the other end ever sent more than it should. */
function copyRef(r: ValueRef): ValueRef {
  if (r.kind === 'environment' || r.kind === 'chatPerson') {
    return { kind: r.kind, ...(r.value !== undefined ? { value: r.value } : {}) };
  }
  return { kind: 'literal', ...(r.prefix !== undefined ? { prefix: r.prefix } : {}) };
}

function copyRefs(m: { [key in string]?: ValueRef } | undefined) {
  if (!m) return undefined;
  const out: { [key in string]?: ValueRef } = {};
  for (const [k, r] of Object.entries(m)) if (r) out[k] = copyRef(r);
  return Object.keys(out).length ? out : undefined;
}

/** What Settings → MCP's "Copy from another PC" may offer: the other PC's entries this PC does
 * not already have (same id = skipped, never overwritten), re-blanked defensively. A blanked
 * secret slot saves as-is and the row then says it needs its token. */
export function copyImportable(remote: McpServer[], existingIds: string[]): McpServer[] {
  return remote
    .filter((s) => !existingIds.includes(s.id))
    .map((s) => {
      const t = s.transport;
      return {
        ...s,
        transport: {
          ...t,
          ...(copyRefs(t.env) ? { env: copyRefs(t.env) } : {}),
          ...(copyRefs(t.headers) ? { headers: copyRefs(t.headers) } : {}),
          ...(t.oauth
            ? {
                oauth: {
                  ...t.oauth,
                  ...(t.oauth.clientSecret ? { clientSecret: copyRef(t.oauth.clientSecret) } : {}),
                },
              }
            : {}),
        },
      };
    });
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
