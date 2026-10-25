// The dev fake host's MCP ops (Settings → MCP in a plain browser). fakeHost.ts registers it
// through `registerFakeOps`. Seeds one connected server and one failing server, so every
// readiness state can be looked at; presets carry no addresses.
//
// Dev builds only. fakeHost.ts imports this file, so this file only imports types from it.

import { ApiError } from '../api/client.ts';
import type { McpPreset, McpServer, RepoServer, ServerStatus } from '../api/mcp.ts';
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
  ];
  const status: ServerStatus[] = [
    { serverId: 'helm_hub', state: 'connected', toolCount: 5, at: Date.now() },
    { serverId: 'files', state: 'failed', error: 'Local files exited (code 1).', at: Date.now() },
  ];
  const repo: RepoServer[] = [
    { folder: 'C:/work/repo', file: 'C:/work/repo/.mcp.json', name: 'docs', hash: 'a'.repeat(64), state: 'held' },
    { folder: 'C:/work/repo', file: 'C:/work/repo/.mcp.json', name: 'github', hash: 'b'.repeat(64), state: 'approved' },
  ];
  const snap = () => ({ revision: revision(), servers: structuredClone(servers) });
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
      default:
        throw new ApiError('refused', 'This operation is not available here.');
    }
  };
  return { prefixes: ['mcp_'], handler };
}
