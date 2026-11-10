// Settings → MCP, the Google accounts block: its ops and the logic with no React in it (what the
// block shows, and how a started sign-in is watched).

import type { Api } from '../../api/client.ts';
import type { GoogleAccountList, GooglePermission, GoogleSignIn, McpEditorSnapshot, McpServer } from '../../api/generated/mcp/index.ts';

export type { GoogleAccount, GoogleAccountList, GooglePermission, GoogleSignIn } from '../../api/generated/mcp/index.ts';

/** The preset a Google accounts server is made from. */
export const GOOGLE_PRESET = 'google_accounts';

/** How often the account list is read while a sign-in is open, and how long it is waited for. */
export const POLL_MS = 2000;
export const SIGN_IN_WINDOW_MS = 180_000;

export const CLIENT_ID_ENV = 'GOOGLE_OAUTH_CLIENT_ID';
export const CLIENT_SECRET_ENV = 'GOOGLE_OAUTH_CLIENT_SECRET';

export const PERMISSION_OPTIONS: { value: GooglePermission; label: string }[] = [
  { value: 'readonly', label: 'Read only' },
  { value: 'drafts', label: 'Read and draft' },
  { value: 'send', label: 'Can send' },
];

export const PERMISSION_HELP: Record<GooglePermission, string> = {
  readonly: 'Chats can read and search mail. They cannot label, draft or send.',
  drafts: 'Chats can read, label and write drafts. They never send.',
  send: 'Chats can also send mail. Use this only if you want a chat to be able to send for you.',
};

export const SIGN_IN_NOTE = 'Your browser opens Google’s sign-in. Helm never sees your password.';
export const REMOVE_NOTE =
  'Helm forgets this account’s sign-in. To revoke it fully, remove Helm’s access at myaccount.google.com/permissions.';
export const NEEDS_UV = 'Install uv (docs.astral.sh/uv) to run this server.';

/** The numbered steps of the one-time setup, shown beside the server. */
export const SETUP_STEPS = [
  'Create a Google Cloud project and turn on the Gmail API.',
  'Set the OAuth consent screen to External and publish it “In production”. Otherwise Google ends each sign-in after 7 days. The “unverified app” warning at sign-in is expected.',
  'Create an OAuth client of type “Desktop app”.',
  'Paste its client id and secret into the server’s environment here, and save.',
];

export function googleOps(api: Api) {
  return {
    accounts: (serverId: string) => api<GoogleAccountList>('mcp_google_accounts', { serverId }),
    /** Starts the sign-in; Google's page opens in the browser. */
    add: (serverId: string, email: string) => api<GoogleSignIn>('mcp_google_add', { serverId, email }),
    remove: (serverId: string, email: string) => api<GoogleAccountList>('mcp_google_remove', { serverId, email }),
    label: (serverId: string, email: string, label: string) => api<GoogleAccountList>('mcp_google_label', { serverId, email, label }),
    /** Rewrites the server's permission argument. */
    permission: (serverId: string, permission: GooglePermission) => api<McpEditorSnapshot>('mcp_google_permission', { serverId, permission }),
  };
}

export type GoogleOps = ReturnType<typeof googleOps>;

export function isGoogleServer(s: McpServer): boolean {
  return s.preset === GOOGLE_PRESET;
}

/** The sign-in client's id and secret are both saved (or typed, in an unsaved draft). */
export function clientSaved(s: McpServer): boolean {
  const env = s.transport.env ?? {};
  return [CLIENT_ID_ENV, CLIENT_SECRET_ENV].every((k) => {
    const r = env[k];
    return !!r && (r.kind === 'literal' ? !!(r.secretId || r.value) : !!r.value);
  });
}

/** Loose check before the host's own: something@something.tld, no spaces. */
export function emailLooksValid(s: string): boolean {
  return /^[^\s@]+@[^\s@]+\.[^\s@]+$/.test(s.trim());
}

export type BlockState =
  | { kind: 'loading' }
  | { kind: 'no-uv'; message: string }
  | { kind: 'no-client'; message: string }
  | { kind: 'empty' }
  | { kind: 'ready' };

/** What the Accounts block leads with: the first thing the user has to fix, else the list. */
export function blockState(server: McpServer, list: GoogleAccountList | null): BlockState {
  if (!list) return { kind: 'loading' };
  if (!list.uvFound) return { kind: 'no-uv', message: NEEDS_UV };
  if (!clientSaved(server)) {
    return { kind: 'no-client', message: 'Add the Google client id and secret to the server’s environment and save, then add accounts.' };
  }
  return list.accounts.length === 0 ? { kind: 'empty' } : { kind: 'ready' };
}

export interface SignInWatch {
  startedAt: number;
  email: string;
}

export type WatchState = 'waiting' | 'added' | 'timeout';

/** One poll of a started sign-in: the address shows up in the list, the time runs out, or neither. */
export function watchState(w: SignInWatch, now: number, accounts: { email: string }[]): WatchState {
  if (accounts.some((a) => a.email.toLowerCase() === w.email.trim().toLowerCase())) return 'added';
  return now - w.startedAt >= SIGN_IN_WINDOW_MS ? 'timeout' : 'waiting';
}
