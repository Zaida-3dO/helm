// Settings → Runtime: the shapes of the host's `runtime_status` and `runtime_settings_*`
// answers (host/crates/chats/src/runtime_settings.rs, helm_workers::status), and the pure helpers
// the pane renders with.

/** One component's status: where it was found and its version, or why a chat cannot use it. */
export interface Found {
  path: string | null;
  source: string | null;
  version: string | null;
  problem: string | null;
  /** A command that fixes the problem (Node too old). Shown to copy; Helm never runs it. */
  upgradeCommand?: string | null;
}

export interface RuntimeStatus {
  node: Found;
  claude: Found;
  /** Everything that needs attention, in words for the user. */
  problems: string[];
}

export type SubagentScope = 'off' | 'orchestrator' | 'all';

export const SUBAGENT_SCOPE_OPTIONS = [
  { value: 'off', title: 'Off', subtitle: 'Sub-agents are never limited' },
  { value: 'orchestrator', title: 'Orchestrator chats', subtitle: 'Only chats with Orchestrator on' },
  { value: 'all', title: 'All chats', subtitle: 'Every chat with Claude Code sub-agents' },
];

export interface RuntimeSettings {
  /** Empty: Helm finds Claude Code itself. */
  executable: string;
  skipPermissions: boolean;
  fastMode: boolean;
  /** Claude Code's own worktree tools (EnterWorktree, ExitWorktree) may be offered to chats. */
  worktreeTools: boolean;
  /** Which chats get turn and context limits on their sub-agents. */
  subagentLimits: SubagentScope;
  /** Refuse a sub-agent's tool calls past the hard limit (otherwise it is only told). */
  subagentHardStop: boolean;
  /** Tool calls before a sub-agent is told to hand off and stop. */
  subagentSoftTurns: number;
  /** Cut very long outputs of Helm's own tools to a head and a tail. */
  toolOutputCap: boolean;
  /** Claude Code's own schedule tools may be offered; each call is redirected to Helm's schedules. */
  scheduleTools: boolean;
  /** Record which files shell commands change, for the turn's change card. */
  shellChanges: boolean;
  /** Agents, their tools and Helm's helpers run at below-normal priority. */
  lowPriority: boolean;
  /** null: Claude Code's default model. */
  model: string | null;
  effort: string;
  responseVerbosity: string;
  /** claude-runtime.json could not be read. */
  problem: string | null;
}

/** One repository in Helm's list (`repo_list`), as far as this pane shows it. */
export interface RepoRow {
  repoId: string;
  remoteUrl?: string;
  worktreesAllowed: boolean;
  locations: { id: string; path: string; label?: string }[];
}

/** The line under a repository: where it is checked out. */
export function repoSummary(r: RepoRow): string {
  const n = r.locations.length;
  const first = r.locations[0]?.path;
  if (n === 0) return 'No checkout listed';
  return n === 1 ? first! : `${first} and ${n - 1} more`;
}

export type SettingsPatch = Partial<Omit<RuntimeSettings, 'problem' | 'model'>> & { model?: string };

const SOURCES: Record<string, string> = {
  HELM_NODE_PATH: 'from HELM_NODE_PATH',
  PATH: 'found on PATH',
  'claude-runtime.json': 'the executable set below',
  HELM_CLAUDE_PATH: 'from HELM_CLAUDE_PATH',
  'local-bin': 'in ~/.local/bin',
  'vscode-extension': "from the VS Code extension",
  HELM_TEST_CLAUDE: 'the test fake (debug build)',
};

export function sourceLabel(source: string | null): string {
  if (!source) return '';
  return SOURCES[source] ?? source;
}

/** The one-line status under a component: ready (version and where) or the problem. */
export function statusLine(f: Found): { ready: boolean; text: string } {
  if (f.problem && !f.path) return { ready: false, text: f.problem };
  const parts = [f.version ?? 'version unknown', sourceLabel(f.source)].filter(Boolean);
  if (f.problem) return { ready: false, text: `${parts.join(', ')}: ${f.problem}` };
  return { ready: true, text: parts.join(', ') };
}

export const DEFAULT_MODEL = 'default';

/** Picker options for the new-chat model: Claude Code's default first, then the catalogue. */
export function modelOptions(models: { id: string; name: string; description?: string }[], current: string | null): { value: string; title: string; subtitle?: string }[] {
  const out: { value: string; title: string; subtitle?: string }[] = [{ value: DEFAULT_MODEL, title: "Claude Code's default" }];
  for (const m of models) {
    if (m.id === DEFAULT_MODEL || out.some((o) => o.value === m.id)) continue;
    out.push({ value: m.id, title: m.name || m.id, subtitle: m.description || undefined });
  }
  // A stored model the catalogue no longer lists still shows, so the picker never lies.
  if (current && !out.some((o) => o.value === current)) out.push({ value: current, title: current });
  return out;
}

export const EFFORT_OPTIONS = [
  { value: 'low', title: 'Low' },
  { value: 'medium', title: 'Medium' },
  { value: 'high', title: 'High' },
  { value: 'xhigh', title: 'Extra high' },
  { value: 'max', title: 'Max' },
];

export const VERBOSITY_OPTIONS = [
  { value: 'low', title: 'Short', subtitle: 'The result in a sentence or two' },
  { value: 'medium', title: 'Medium', subtitle: 'The result, then the detail that matters' },
  { value: 'high', title: 'Full', subtitle: "Claude Code's own length" },
];

/** Picker options for the sub-agent turn limit; a stored value that is not listed still shows. */
export function turnOptions(current: number | null): { value: string; title: string }[] {
  const values = [40, 60, 100, 150, 250, 500];
  if (current !== null && !values.includes(current)) values.push(current);
  return values.sort((a, b) => a - b).map((n) => ({ value: String(n), title: `${n} tool calls` }));
}

export const PERMISSION_OPTIONS = [
  { value: 'bypass', title: 'Bypass permissions', subtitle: 'Runs every tool without asking' },
  { value: 'ask', title: 'Asks permission', subtitle: 'Asks before it edits or runs anything' },
];

/** What `claude_token_status` answers: whether chats have a saved Claude token and when it was
 * saved (Unix ms). The token itself never reaches the window. */
export interface ClaudeTokenStatus {
  saved: boolean;
  savedAt?: number;
}

const DAY_MS = 86_400_000;
/** A token made by `claude setup-token` lasts a year. */
const TOKEN_LIFE_DAYS = 365;
/** How long before it runs out the row starts to say so. */
const TOKEN_WARN_DAYS = 14;

export type TokenRowState = 'checking' | 'none' | 'saved' | 'expiring' | 'expired';

/** What the sign-in row says for the saved token, as of `now`. */
export function tokenRow(status: ClaudeTokenStatus | null, now: number): { state: TokenRowState; title: string; note: string } {
  if (!status) return { state: 'checking', title: 'Chats sign in with a saved Claude token', note: 'Checking…' };
  if (!status.saved) {
    return {
      state: 'none',
      title: "Chats use this PC's Claude sign-in",
      note: 'Many chats can refresh that one sign-in at the same moment and trip over each other. Save a long-lived Claude token and chats stop refreshing it. Sign in with a token opens a terminal tab and Helm picks the token up when it appears; you can also paste one.',
    };
  }
  const savedAt = status.savedAt;
  const day = savedAt === undefined ? '' : new Date(savedAt).toLocaleDateString('en-GB', { day: 'numeric', month: 'short', year: 'numeric' });
  const title = `Chats sign in with a saved Claude token${day ? ` (saved ${day})` : ''}`;
  if (savedAt === undefined) return { state: 'saved', title, note: '' };
  const daysLeft = Math.ceil((savedAt + TOKEN_LIFE_DAYS * DAY_MS - now) / DAY_MS);
  if (daysLeft <= 0) return { state: 'expired', title, note: 'This token has run out. Sign in again to make a new one.' };
  if (daysLeft <= TOKEN_WARN_DAYS) {
    return { state: 'expiring', title, note: `This token runs out in ${daysLeft} ${daysLeft === 1 ? 'day' : 'days'}. Sign in again to make a new one.` };
  }
  return { state: 'saved', title, note: 'Only the usage meter still uses the normal Claude sign-in.' };
}

/** Whether pasted text can be a Claude token (the host checks again): one run of letters, digits,
 * dashes and underscores starting `sk-ant-`, and not a pay-as-you-go API key. */
export function looksLikeToken(text: string): boolean {
  const t = text.trim();
  return /^sk-ant-[A-Za-z0-9_-]{13,}$/.test(t) && !t.startsWith('sk-ant-api');
}
