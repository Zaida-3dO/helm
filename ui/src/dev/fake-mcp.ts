// The dev fake host's MCP ops (Settings → MCP in a plain browser). fakeHost.ts registers it
// through `registerFakeOps`. Seeds one connected server and one failing server, so every
// readiness state can be looked at; presets carry no addresses.
//
// Dev builds only. fakeHost.ts imports this file, so this file only imports types from it.

import { ApiError } from '../api/client.ts';
import type { McpPreset, McpServer, RepoServer, ServerStatus } from '../api/mcp.ts';
import type { GoogleAccount, GoogleAccountList, GooglePermission } from '../api/generated/mcp/index.ts';
import type { FakeOpHandler } from './fakeHost.ts';

const PRESETS: McpPreset[] = [
  ['agent_standup', 'Agent Standup', 'The task board.', 'https://your-board.example/mcp', false, ['get_item', 'record', 'ownership']],
  ['home_assistant', 'Home Assistant', 'Home Assistant’s MCP server.', 'http://homeassistant.local:8123/mcp', true, []],
  ['helm_hub', 'Helm hub memory', 'Shared memory on the Helm hub.', 'http://your-hub.example/mcp', true, ['memory_search']],
].map(([id, name, description, urlPlaceholder, suggestPrivateHttp, expectTools]) => ({
  id: id as string,
  name: name as string,
  description: description as string,
  transport: 'http',
  urlPlaceholder: urlPlaceholder as string,
  tokenHeader: 'Authorization',
  tokenPrefix: 'Bearer ',
  suggestPrivateHttp: suggestPrivateHttp as boolean,
  expectTools: expectTools as string[],
  guidance: '',
}));
PRESETS.push({
  id: 'google_accounts',
  name: 'Google accounts (Gmail and Calendar)',
  description: 'Several Gmail and Calendar accounts, signed in with your own Google client.',
  transport: 'stdio',
  urlPlaceholder: '',
  tokenHeader: '',
  tokenPrefix: '',
  suggestPrivateHttp: false,
  expectTools: [],
  guidance: '',
  command: 'uvx.exe',
  args: ['workspace-mcp', '--tools', 'gmail', '--permissions', 'gmail:drafts'],
  secretEnv: ['GOOGLE_OAUTH_CLIENT_ID', 'GOOGLE_OAUTH_CLIENT_SECRET'],
});

export function mcpFakeOps(): { prefixes: string[]; handler: FakeOpHandler } {
  let rev = 1;
  const revision = () => rev.toString(16).padStart(64, '0');
  const servers: McpServer[] = [
    {
      id: 'helm_hub',
      name: 'Helm hub memory',
      enabled: true,
      preset: 'helm_hub',
      allowPrivateHttp: true,
      transport: { type: 'http', url: 'http://10.0.0.2:8080/mcp', headers: { Authorization: { kind: 'literal', secretId: `lit_${'0'.repeat(32)}`, prefix: 'Bearer ' } } },
    },
    { id: 'files', name: 'Local files', enabled: true, allowPrivateHttp: false, transport: { type: 'stdio', command: 'node.exe', args: ['server.js'] } },
    { id: 'crm', name: 'Example CRM', enabled: true, allowPrivateHttp: false, transport: { type: 'http', url: 'https://crm.example/mcp', oauth: {} } },
  ];
  const status: ServerStatus[] = [
    { serverId: 'helm_hub', state: 'connected', toolCount: 5, at: Date.now() },
    { serverId: 'files', state: 'failed', error: 'Local files exited (code 1).', at: Date.now() },
    { serverId: 'crm', state: 'signin_required', error: 'Not connected: open Settings > MCP and click Connect.', at: Date.now() },
  ];
  // Which browser-signed-in servers have a sign-in kept (the fake answers instantly).
  const oauthSigned: Record<string, boolean> = {};
  const repo: RepoServer[] = [
    { folder: 'C:/work/repo', file: 'C:/work/repo/.mcp.json', name: 'docs', hash: 'a'.repeat(64), state: 'held' },
    { folder: 'C:/work/repo', file: 'C:/work/repo/.mcp.json', name: 'github', hash: 'b'.repeat(64), state: 'approved' },
  ];
  const snap = () => ({ revision: revision(), servers: structuredClone(servers) });
  // The Google accounts dialog: the sign-in client (set or not), a few addresses in memory,
  // and a fake browser sign-in that lands a demo account after 4 s.
  const accounts: GoogleAccount[] = [];
  let googleClient = false;
  let googleSigningIn = false;
  // Same shape as the host's google.ts: the level is absent when the arguments set one the
  // picker does not know.
  const calFlag = (args: string[] | undefined): string | undefined =>
    args?.flatMap((x) => (x === 'calendar' || x.startsWith('calendar:') ? [x] : x.startsWith('--permissions=') ? x.slice(14).split(',').filter((v) => v === 'calendar' || v.startsWith('calendar:')) : []))[0];
  const googleList = (): GoogleAccountList => {
    const server = servers.find((x) => x.preset === 'google_accounts');
    const level = server?.transport.args?.find((x) => x.startsWith('gmail:'))?.slice(6);
    const permission = (['readonly', 'drafts', 'send'] as GooglePermission[]).find((p) => p === level);
    const cal = calFlag(server?.transport.args);
    const calendar =
      cal === undefined ? 'off' : cal === 'calendar:readonly' ? 'readonly' : cal === 'calendar:full' ? 'edit_events' : undefined;
    // Same rule as the host: the bridge runs for a server carrying its own client env, or for
    // one whose arguments ask for a known calendar level once the profile's client is saved.
    const legacy =
      !!server &&
      (server.transport.args?.[0] ?? '').startsWith('workspace-mcp') &&
      !!server.transport.env?.GOOGLE_OAUTH_CLIENT_ID &&
      !!server.transport.env?.GOOGLE_OAUTH_CLIENT_SECRET;
    const bridge = legacy || (calendar !== undefined && calendar !== 'off' && googleClient);
    return {
      accounts: structuredClone(accounts),
      ...(permission ? { permission } : {}),
      ...(calendar ? { calendar } : {}),
      uvFound: true,
      signingIn: googleSigningIn,
      hostServed: !bridge,
    };
  };
  const check = (a: Record<string, unknown>) => {
    if (typeof a.revision === 'string' && a.revision !== revision()) throw new ApiError('conflict', 'The MCP settings changed since they were read.');
  };
  const handler: FakeOpHandler = (op, a) => {
    switch (op) {
      case 'mcp_config_get':
        return snap();
      case 'mcp_presets_list':
        return PRESETS;
      case 'mcp_servers_status':
        return status;
      case 'mcp_chat_problems':
        // Same shape as the host's: only failed and sign-in servers come back, named, with words for the person.
        return status
          .filter((s) => s.state === 'failed' || s.state === 'signin_required')
          .map((s) => ({
            serverId: s.serverId,
            name: servers.find((x) => x.id === s.serverId)?.name ?? s.serverId,
            state: s.state,
            reason: s.error ?? (s.state === 'failed' ? 'The last connection failed.' : 'Needs you to sign in.'),
          }));
      case 'mcp_repo_servers_list':
        return repo;
      case 'mcp_repo_approve':
      case 'mcp_repo_revoke': {
        const r = repo.find((x) => x.file === a.file && x.name === a.name);
        if (r && r.state !== 'reserved') r.state = op === 'mcp_repo_approve' ? 'approved' : 'held';
        return repo;
      }
      case 'mcp_server_test': {
        const t = (a.server as McpServer).transport;
        const url = t.url ?? '';
        if (url.includes('fail')) return { ok: false, message: 'Could not connect: connection refused', signinRequired: false };
        if (url.includes('auth')) return { ok: false, message: 'The server answered 401: it needs a sign-in or a different token. Check the headers.', signinRequired: true };
        return { ok: true, message: 'Connected to fake-server 1.0. It offers 3 tools.', serverName: 'fake-server', toolCount: 3, signinRequired: false };
      }
      case 'mcp_server_save': {
        check(a);
        const s = structuredClone(a.server as McpServer);
        for (const refs of [s.transport.headers, s.transport.env]) {
          for (const r of Object.values(refs ?? {})) {
            if (r?.kind === 'literal' && r.value) {
              r.secretId = `lit_${rev.toString(16).padStart(32, '0')}`;
              delete r.value;
            }
          }
        }
        const i = servers.findIndex((x) => x.id === s.id);
        if (i >= 0) servers[i] = s;
        else servers.push(s);
        rev++;
        return snap();
      }
      case 'mcp_copy_import': {
        check(a);
        // Same rule as the host: an id this PC already has is skipped, never overwritten.
        for (const s of structuredClone((a.servers ?? []) as McpServer[])) {
          if (!servers.some((x) => x.id === s.id)) servers.push(s);
        }
        rev++;
        return snap();
      }
      case 'mcp_server_remove': {
        check(a);
        const i = servers.findIndex((x) => x.id === a.serverId);
        if (i < 0) throw new ApiError('bad_args', 'That server no longer exists.');
        servers.splice(i, 1);
        rev++;
        return snap();
      }
      case 'mcp_server_set_enabled': {
        check(a);
        const s = servers.find((x) => x.id === a.serverId);
        if (!s) throw new ApiError('bad_args', 'That server no longer exists.');
        s.enabled = a.enabled === true;
        rev++;
        return snap();
      }
      case 'mcp_oauth_status':
        return { connected: oauthSigned[String(a.serverId)] === true };
      case 'mcp_oauth_connect': {
        // The fake browser round-trip answers at once.
        oauthSigned[String(a.serverId)] = true;
        const st = status.find((x) => x.serverId === String(a.serverId));
        if (st) Object.assign(st, { state: 'connected', toolCount: 2, error: undefined, at: Date.now() });
        return { connected: true };
      }
      case 'mcp_oauth_disconnect': {
        delete oauthSigned[String(a.serverId)];
        const st = status.find((x) => x.serverId === String(a.serverId));
        if (st) Object.assign(st, { state: 'signin_required', error: 'Not connected: open Settings > MCP and click Connect.', at: Date.now() });
        return null;
      }
      case 'mcp_google_accounts':
        return googleList();
      case 'mcp_google_client_get':
        return { configured: googleClient };
      case 'mcp_google_client_set': {
        if (!String(a.clientId ?? '').trim() || !String(a.clientSecret ?? '').trim()) throw new ApiError('bad_args', 'The Google client id is not usable.');
        googleClient = true;
        return { configured: true };
      }
      case 'mcp_google_add': {
        if (!googleClient) throw new ApiError('bad_args', 'Set the Google sign-in client in Settings → MCP first.');
        if (googleSigningIn) throw new ApiError('bad_args', 'A sign-in is already open. Finish it in your browser, or try again in a few minutes.');
        googleSigningIn = true;
        setTimeout(() => {
          googleSigningIn = false;
          accounts.push({ email: `demo.account${accounts.length + 1}@gmail.com` });
        }, 4000);
        return { authUrl: 'https://accounts.google.com/o/oauth2/auth?fake=1' };
      }
      case 'mcp_google_remove': {
        const i = accounts.findIndex((x) => x.email.toLowerCase() === String(a.email).toLowerCase());
        if (i < 0) throw new ApiError('bad_args', 'That account is not signed in.');
        accounts.splice(i, 1);
        return googleList();
      }
      case 'mcp_google_label': {
        const acc = accounts.find((x) => x.email.toLowerCase() === String(a.email).toLowerCase());
        if (!acc) throw new ApiError('bad_args', 'That account is not signed in.');
        const label = String(a.label).trim();
        if (label) acc.label = label;
        else delete acc.label;
        return googleList();
      }
      case 'mcp_google_permission': {
        const s = servers.find((x) => x.id === a.serverId && x.preset === 'google_accounts');
        if (!s) throw new ApiError('bad_args', 'That is not a Google accounts server.');
        const args = (s.transport.args ?? []).filter((x) => !x.startsWith('gmail:'));
        s.transport.args = [...args, `gmail:${String(a.permission)}`];
        rev++;
        return snap();
      }
      case 'mcp_google_calendar': {
        const s = servers.find((x) => x.id === a.serverId && x.preset === 'google_accounts');
        if (!s) throw new ApiError('bad_args', 'That is not a Google accounts server.');
        const args = s.transport.args ?? [];
        const level = String(a.calendar);
        const gmail = args.find((x) => x.startsWith('gmail:'));
        const kept = args.filter((x) => x !== '--permissions' && !x.startsWith('gmail:') && x !== 'calendar' && !x.startsWith('calendar:'));
        const adds = [
          ...(gmail ? [gmail] : []),
          ...(level === 'off' ? [] : [level === 'edit_events' ? 'calendar:full' : 'calendar:readonly']),
        ];
        s.transport.args = adds.length ? [...kept, '--permissions', ...adds] : kept;
        rev++;
        return snap();
      }
      default:
        throw new ApiError('refused', 'This operation is not available here.');
    }
  };
  return { prefixes: ['mcp_'], handler };
}
