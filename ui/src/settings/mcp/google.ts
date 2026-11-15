// Settings → MCP, the Google accounts dialog: its ops and the logic with no React in it (what
// the dialog shows, how the sign-in client is checked, and how a started sign-in is watched).
// One sign-in client per profile is kept in the credential store by the host: the UI only ever
// asks whether one is set and hands over the two values once.

import type { Api } from '../../api/client.ts';
import type { GoogleAccountList, GoogleClientState, GooglePermission, GoogleSignIn, McpEditorSnapshot, McpServer } from '../../api/generated/mcp/index.ts';

export type { GoogleAccount, GoogleAccountList, GoogleClientState, GooglePermission, GoogleSignIn } from '../../api/generated/mcp/index.ts';

/** The preset a Google accounts server is made from. */
export const GOOGLE_PRESET = 'google_accounts';

/** How often the account list is read while a sign-in is open, and how long it is waited for. */
export const POLL_MS = 2000;
export const SIGN_IN_WINDOW_MS = 180_000;

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
export const UNVERIFIED_NOTE =
  'The “Google hasn’t verified this app” warning at sign-in is expected: the client belongs to you, not to Google. Choose Advanced, then Go to the app.';

/** One step of the one-time Google Cloud setup, with the console page that shows it. */
export interface ConsoleStep {
  label: string;
  url: string;
}

/** The six steps, in the order the Google console wants them. Each link opens in the system browser. */
export const CONSOLE_STEPS: ConsoleStep[] = [
  { label: 'Create a Google Cloud project.', url: 'https://console.cloud.google.com/projectcreate' },
  { label: 'Turn on the Gmail API for that project.', url: 'https://console.cloud.google.com/apis/library/gmail.googleapis.com' },
  { label: 'Fill in the OAuth consent screen: an app name and a support email.', url: 'https://console.cloud.google.com/auth/branding' },
  {
    label: 'Set the audience to External, then publish the app (“In production”). While it is still “Testing”, Google ends each sign-in after 7 days.',
    url: 'https://console.cloud.google.com/auth/audience',
  },
  { label: 'Under data access, add the scopes for reading, labelling and drafting mail (and sending, if you want that level).', url: 'https://console.cloud.google.com/auth/scopes' },
  { label: 'Create an OAuth client of type “Desktop app”, and copy its client id and secret into the two fields below.', url: 'https://console.cloud.google.com/auth/clients/create' },
];

export function googleOps(api: Api) {
  return {
    accounts: (serverId: string) => api<GoogleAccountList>('mcp_google_accounts', { serverId }),
    /** Starts the sign-in; Google's page opens in the browser. Without an address Google shows its chooser. */
    add: (serverId: string) => api<GoogleSignIn>('mcp_google_add', { serverId }),
    remove: (serverId: string, email: string) => api<GoogleAccountList>('mcp_google_remove', { serverId, email }),
    label: (serverId: string, email: string, label: string) => api<GoogleAccountList>('mcp_google_label', { serverId, email, label }),
    /** Rewrites the server's permission argument. */
    permission: (serverId: string, permission: GooglePermission) => api<McpEditorSnapshot>('mcp_google_permission', { serverId, permission }),
    /** Whether this profile already has a sign-in client. The values themselves never appear. */
    clientGet: () => api<GoogleClientState>('mcp_google_client_get', {}),
    /** Saves the client once. Write-only: a saved secret is never read back. */
    clientSet: (clientId: string, clientSecret: string) => api<GoogleClientState>('mcp_google_client_set', { clientId, clientSecret }),
  };
}

export type GoogleOps = ReturnType<typeof googleOps>;

export function isGoogleServer(s: McpServer): boolean {
  return s.preset === GOOGLE_PRESET;
}

/** Both fields filled in, before the host's own stricter check. */
export function clientLooksSet(clientId: string, secret: string): boolean {
  return clientId.trim().length > 0 && secret.trim().length > 0;
}

export type BlockState =
  | { kind: 'loading' }
  | { kind: 'no-uv'; message: string }
  | { kind: 'no-client' }
  | { kind: 'empty' }
  | { kind: 'ready' };

/** What the dialog leads with: the first thing the user has to fix, else the accounts. */
export function blockState(list: GoogleAccountList | null, clientConfigured: boolean | null): BlockState {
  if (!list || clientConfigured === null) return { kind: 'loading' };
  if (!list.uvFound) return { kind: 'no-uv', message: NEEDS_UV };
  if (!clientConfigured) return { kind: 'no-client' };
  return list.accounts.length === 0 ? { kind: 'empty' } : { kind: 'ready' };
}

export interface SignInWatch {
  startedAt: number;
  /** The addresses that were already there when the sign-in started, lower-cased. */
  known: string[];
}

export type WatchState = 'waiting' | 'added' | 'timeout';

/** One poll of a started sign-in: an address new to the list arrived, the time ran out, or neither. */
export function watchState(w: SignInWatch, now: number, accounts: { email: string }[]): WatchState {
  if (accounts.some((a) => !w.known.includes(a.email.toLowerCase()))) return 'added';
  return now - w.startedAt >= SIGN_IN_WINDOW_MS ? 'timeout' : 'waiting';
}
