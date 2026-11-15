// Settings → MCP, the browser sign-in for a server at an address: its ops and the small rules.
// Nothing here ever sees token material — the status only says whether a sign-in is kept.

import type { Api } from '../../api/client.ts';
import type { McpServer, OAuthStatus } from '../../api/generated/mcp/index.ts';

export type { OAuthStatus } from '../../api/generated/mcp/index.ts';

/** What the row says while a browser sign-in is open. */
export const WAITING_NOTE = 'Your browser is open: finish signing in there. This waits until it answers, and Helm never sees your password.';

export function oauthOps(api: Api) {
  return {
    /** Opens the browser sign-in; resolves when it finishes, refuses while it can still be retried. */
    connect: (serverId: string) => api<OAuthStatus>('mcp_oauth_connect', { serverId }),
    status: (serverId: string) => api<OAuthStatus>('mcp_oauth_status', { serverId }),
    /** Forgets the kept sign-in (nothing is revoked at the server). Answers nothing. */
    forget: (serverId: string) => api<null>('mcp_oauth_disconnect', { serverId }),
  };
}

export type OAuthOps = ReturnType<typeof oauthOps>;

/** True for a server at an address that signs in through the browser. */
export function isOAuthServer(s: McpServer): boolean {
  return s.transport.type !== 'stdio' && !!s.transport.oauth;
}

/** True when a sign-in is kept for the server, so Disconnect is worth offering. */
export function canDisconnect(status: OAuthStatus | undefined): boolean {
  return !!status?.connected;
}

/** The button a row offers for browser sign-in, or nothing when there is nothing to offer. */
export function connectLabel(status: OAuthStatus | undefined, serverState: string | undefined): 'Connect' | 'Sign in again' | undefined {
  if (serverState === 'signin_required') return 'Sign in again';
  return status?.connected ? undefined : 'Connect';
}
