// Typed wrappers over the MCP ops. Every call still goes through
// api(op, args).

import type { Api } from './client.ts';
import type { McpEditorSnapshot, McpPreset, McpServer, RepoServer, ServerStatus } from './generated/mcp/index.ts';

/** "Use my Claude Code MCP servers" (user-only; on by default). */
export const NATIVE_MCP_PREF = 'nativeMcp';

export type {
  McpEditorSnapshot,
  McpPreset,
  McpServer,
  RefKind,
  RepoServer,
  RepoServerState,
  ServerState,
  ServerStatus,
  Transport,
  TransportKind,
  ValueRef,
} from './generated/mcp/index.ts';

export function mcpOps(api: Api) {
  return {
    get: () => api<McpEditorSnapshot>('mcp_config_get', {}),
    save: (server: McpServer, revision?: string) => api<McpEditorSnapshot>('mcp_server_save', { server, ...(revision ? { revision } : {}) }),
    remove: (serverId: string, revision?: string) =>
      api<McpEditorSnapshot>('mcp_server_remove', { serverId, ...(revision ? { revision } : {}) }),
    setEnabled: (serverId: string, enabled: boolean, revision?: string) =>
      api<McpEditorSnapshot>('mcp_server_set_enabled', { serverId, enabled, ...(revision ? { revision } : {}) }),
    presets: () => api<McpPreset[]>('mcp_presets_list', {}),
    status: () => api<ServerStatus[]>('mcp_servers_status', {}),
    repoList: () => api<RepoServer[]>('mcp_repo_servers_list', {}),
    repoApprove: (s: RepoServer) => api<RepoServer[]>('mcp_repo_approve', { file: s.file, name: s.name, hash: s.hash }),
    repoRevoke: (s: RepoServer) => api<RepoServer[]>('mcp_repo_revoke', { file: s.file, name: s.name }),
    nativeOn: async () => (await api<unknown>('preference_get', { key: NATIVE_MCP_PREF })) !== false,
    setNative: (on: boolean) => api<unknown>('preference', { key: NATIVE_MCP_PREF, value: on }),
  };
}

export type McpOps = ReturnType<typeof mcpOps>;
