// Typed wrappers over the MCP ops. Every call still goes through
// api(op, args).

import type { Api } from './client.ts';
import type { ChatMcpProblem, DoctorOutcome, McpEditorSnapshot, McpPreset, McpServer, RepoServer, ServerStatus, TestOutcome } from './generated/mcp/index.ts';

/** One browser program found on this PC: its name and its executable path. */
export interface BrowserInstall {
  label: string;
  path: string;
}

/** "Use my Claude Code MCP servers" (user-only; on by default). */
export const NATIVE_MCP_PREF = 'nativeMcp';

/** The MCP master switch (user-only; on by default). Off, no chat loads any MCP server. */
export const MCP_ENABLED_PREF = 'mcpEnabled';

export type {
  ChatMcpProblem,
  DoctorCheck,
  DoctorOutcome,
  McpEditorSnapshot,
  McpPreset,
  McpServer,
  RefKind,
  RepoServer,
  RepoServerState,
  ServerState,
  ServerStatus,
  TestOutcome,
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
    /** Connect once to a draft server and list its tools; saves nothing. */
    test: (server: McpServer) => api<TestOutcome>('mcp_server_test', { server }),
    presets: () => api<McpPreset[]>('mcp_presets_list', {}),
    status: () => api<ServerStatus[]>('mcp_servers_status', {}),
    /** The one chat's tool servers it cannot use right now, with reason words for the person. */
    chatProblems: (chatId: string) => api<ChatMcpProblem[]>('mcp_chat_problems', { chatId }),
    /** The broker's own checks for the built-in browser server; changes nothing. */
    doctor: (serverId: string) => api<DoctorOutcome>('mcp_broker_doctor', { serverId }),
    /** The browser programs this PC has in their usual places (plain path checks; empty on
     * other systems). For the browser preset's program row; nothing runs. */
    findBrowsers: () => api<BrowserInstall[]>('mcp_find_browsers', {}),
    /** Servers copied from another PC (brought across by `devices_mcp_list`): an id this PC
     *  already has is skipped, and a blanked secret waits to be entered. */
    importCopy: (servers: McpServer[]) => api<McpEditorSnapshot>('mcp_copy_import', { servers }),
    repoList: () => api<RepoServer[]>('mcp_repo_servers_list', {}),
    repoApprove: (s: RepoServer) => api<RepoServer[]>('mcp_repo_approve', { file: s.file, name: s.name, hash: s.hash }),
    repoRevoke: (s: RepoServer) => api<RepoServer[]>('mcp_repo_revoke', { file: s.file, name: s.name }),
    nativeOn: async () => (await api<unknown>('preference_get', { key: NATIVE_MCP_PREF })) !== false,
    setNative: (on: boolean) => api<unknown>('preference', { key: NATIVE_MCP_PREF, value: on }),
    allOn: async () => (await api<unknown>('preference_get', { key: MCP_ENABLED_PREF })) !== false,
    setAll: (on: boolean) => api<unknown>('preference', { key: MCP_ENABLED_PREF, value: on }),
  };
}

export type McpOps = ReturnType<typeof mcpOps>;
