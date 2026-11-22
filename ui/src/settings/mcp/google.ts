// Settings → MCP, the Google accounts dialog: its ops and the logic with no React in it (what
// the dialog shows, how the sign-in client is checked, and how a started sign-in is watched).
// One sign-in client per profile is kept in the credential store by the host: the UI only ever
// asks whether one is set and hands over the two values once.

import type { Api } from '../../api/client.ts';
import type { CalendarPermission, GoogleAccount, GoogleAccountList, GoogleClientState, GooglePermission, GoogleSignIn, McpEditorSnapshot, McpServer } from '../../api/generated/mcp/index.ts';

export type { CalendarPermission, GoogleAccount, GoogleAccountList, GoogleClientState, GooglePermission, GoogleSignIn } from '../../api/generated/mcp/index.ts';

/** The preset a Google accounts server is made from. */
export const GOOGLE_PRESET = 'google_accounts';

/** How often the account list is read while a sign-in is open, and how long it is waited for.
 * The window matches the host's own listener (5 minutes): the dialog never gives up before the
 * host does, so "Add account again" is never answered with "A sign-in is already open". */
export const POLL_MS = 2000;
export const SIGN_IN_WINDOW_MS = 300_000;

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

/** The calendar level, beside the mail one. The host maps these to the bridge's calendar
 * permission values and its Google scopes; `off` loads no calendar service at all. */
export const CALENDAR_OPTIONS: { value: CalendarPermission; label: string }[] = [
  { value: 'off', label: 'Off' },
  { value: 'readonly', label: 'Read only' },
  { value: 'edit_events', label: 'Edit events' },
];

export const CALENDAR_HELP: Record<CalendarPermission, string> = {
  off: 'No calendar tools: chats only get mail.',
  readonly: 'Chats can read calendars and events. They cannot change anything.',
  edit_events: 'Chats can also create, change and delete events, the same way any other tool use is allowed.',
};

export const SIGN_IN_NOTE = 'Your browser opens Google’s sign-in. Helm never sees your password.';
export const REMOVE_NOTE =
  'Helm forgets this account’s sign-in. To revoke it fully, remove Helm’s access at myaccount.google.com/permissions.';
export const NEEDS_UV = 'Install uv (docs.astral.sh/uv) to run this server.';
/** Sign-in asked for the scopes of the level that was picked at the time; changing the level
 * does not re-ask Google, so each account has to sign in again to reach the new one. */
export const RAISE_NOTE = 'Raising the level: sign each account in again.';
export const SIGNED_IN_AGAIN = 'Signed in again.';
export const STILL_MISSING = 'Google still left some permissions off. Sign in again and tick every box.';
/** The words under an account that has to sign in again: which services lack permissions. */
export function needsSignInWords(a: GoogleAccount): string {
  const names = a.needsSignIn.map((s) => (s === 'calendar' ? 'Calendar' : s === 'gmail' ? 'Gmail' : s));
  return `Needs sign-in again · ${names.join(', ')}`;
}
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
  { label: 'Turn on the Gmail API for that project (and the Google Calendar API, if chats will use Calendar).', url: 'https://console.cloud.google.com/apis/library/gmail.googleapis.com' },
  { label: 'Fill in the OAuth consent screen: an app name and a support email.', url: 'https://console.cloud.google.com/auth/branding' },
  {
    label: 'Set the audience to External, then publish the app (“In production”). While it is still “Testing”, Google ends each sign-in after 7 days.',
    url: 'https://console.cloud.google.com/auth/audience',
  },
  { label: 'Under data access, add the scopes for reading, labelling, drafting and modifying mail (and sending too, if you want that level), and the calendar ones if chats will use Calendar.', url: 'https://console.cloud.google.com/auth/scopes' },
  { label: 'Create an OAuth client of type “Desktop app”, and copy its client id and secret into the two fields below.', url: 'https://console.cloud.google.com/auth/clients/create' },
];

export function googleOps(api: Api) {
  return {
    accounts: (serverId: string) => api<GoogleAccountList>('mcp_google_accounts', { serverId }),
    /** Starts the sign-in; Google's page opens in the browser. Without an address Google shows its chooser;
     * with one (signing an account in again) the page is pre-filled with it. */
    add: (serverId: string, email?: string) => api<GoogleSignIn>('mcp_google_add', email ? { serverId, email } : { serverId }),
    remove: (serverId: string, email: string) => api<GoogleAccountList>('mcp_google_remove', { serverId, email }),
    label: (serverId: string, email: string, label: string) => api<GoogleAccountList>('mcp_google_label', { serverId, email, label }),
    /** Rewrites the server's permission argument. */
    permission: (serverId: string, permission: GooglePermission) => api<McpEditorSnapshot>('mcp_google_permission', { serverId, permission }),
    /** Rewrites the server's calendar level. */
    calendar: (serverId: string, calendar: CalendarPermission) => api<McpEditorSnapshot>('mcp_google_calendar', { serverId, calendar }),
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

/** The env names a legacy Google server kept its own sign-in client in. A server carrying them
 * keeps the external bridge program; every other Google server is served by Helm's own tools,
 * and the worker never starts a process for it. */
export const GOOGLE_CLIENT_ENV = ['GOOGLE_OAUTH_CLIENT_ID', 'GOOGLE_OAUTH_CLIENT_SECRET'];

export function keepsBridge(s: McpServer): boolean {
  return (
    s.transport.type === 'stdio' &&
    (s.transport.args?.[0] ?? '').startsWith('workspace-mcp') &&
    GOOGLE_CLIENT_ENV.every((k) => !!s.transport.env?.[k])
  );
}

/** The calendar level the server's own arguments ask for, as the host reads it: absent is off,
 * an unrecognised value is `null` (the host guesses nothing). */
export function calendarInArgs(s: McpServer): CalendarPermission | null {
  const args = s.transport.type === 'stdio' ? s.transport.args ?? [] : [];
  let value: string | undefined;
  for (const a of args) {
    if (a === 'calendar' || a.startsWith('calendar:')) value = a;
  }
  if (!value) {
    for (const a of args) {
      const joined = a.startsWith('--permissions=') ? a.slice('--permissions='.length) : null;
      const part = joined?.split(',').find((v) => v === 'calendar' || v.startsWith('calendar:'));
      if (part) {
        value = part;
        break;
      }
    }
  }
  if (!value) return 'off';
  if (value === 'calendar:readonly') return 'readonly';
  if (value === 'calendar:full') return 'edit_events';
  if (value === 'calendar') return null;
  return null;
}

/** Whether a bridge process runs for this Google server: either it carries its own client env,
 * or its arguments ask for a calendar level the host knows and is not off (the host starts the
 * bridge for it when the profile's sign-in client is saved). */
export function runsBridge(s: McpServer): boolean {
  if (keepsBridge(s)) return true;
  const cal = calendarInArgs(s);
  return cal !== null && cal !== 'off';
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

/** What the dialog leads with: the first thing the user has to fix, else the accounts. A
 * host-served server needs no program of its own, so a missing `uvx` says nothing about it —
 * only a legacy bridge server has to have one. */
export function blockState(list: GoogleAccountList | null, clientConfigured: boolean | null): BlockState {
  if (!list || clientConfigured === null) return { kind: 'loading' };
  if (!list.hostServed && !list.uvFound) return { kind: 'no-uv', message: NEEDS_UV };
  if (!clientConfigured) return { kind: 'no-client' };
  return list.accounts.length === 0 ? { kind: 'empty' } : { kind: 'ready' };
}

export interface SignInWatch {
  startedAt: number;
  /** The addresses that were already there when the sign-in started, lower-cased. */
  known: string[];
  /** True from the start: the host held its sign-in open the moment `add` returned. A poll that
   * finds it false with no new address was a Cancel or a Deny at Google. */
  sawSigningIn: boolean;
  /** Set when an account already in the list is being signed in again: the address. */
  again?: string;
}

export type WatchState = 'waiting' | 'added' | 'renewed' | 'refused' | 'timeout';

/** One poll of a started sign-in: an address new to the list arrived, the host's listener let
 * go without one (Cancel or Deny at Google — the wait ends there, not at the window), the time
 * ran out, or neither. */
export function watchState(w: SignInWatch, now: number, list: GoogleAccountList): WatchState {
  if (list.accounts.some((a) => !w.known.includes(a.email.toLowerCase()))) return 'added';
  if (list.signingIn) w.sawSigningIn = true;
  else if (w.sawSigningIn) {
    // Signing an account in again ends with the account no longer marked. If it still is, the
    // page was cancelled or some boxes were left unticked.
    const again = w.again?.toLowerCase();
    const mark = again ? list.accounts.find((a) => a.email.toLowerCase() === again)?.needsSignIn.length : undefined;
    return again && mark === 0 ? 'renewed' : 'refused';
  }
  return now - w.startedAt >= SIGN_IN_WINDOW_MS ? 'timeout' : 'waiting';
}
