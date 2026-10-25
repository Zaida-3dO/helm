// Settings → Runtime: the shapes of the host's `runtime_status` and `runtime_settings_*`
// answers (host/crates/chats/src/runtime_settings.rs, helm_workers::status), and the pure helpers
// the pane renders with.

/** One component's status: where it was found and its version, or why a chat cannot use it. */
export interface Found {
  path: string | null;
  source: string | null;
  version: string | null;
  problem: string | null;
}

export interface RuntimeStatus {
  node: Found;
  claude: Found;
  /** Everything that needs attention, in words for the user. */
  problems: string[];
}

export interface RuntimeSettings {
  /** Empty: Helm finds Claude Code itself. */
  executable: string;
  skipPermissions: boolean;
  fastMode: boolean;
  /** Claude Code's own worktree tools (EnterWorktree, ExitWorktree) may be offered to chats. */
  worktreeTools: boolean;
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

export const PERMISSION_OPTIONS = [
  { value: 'bypass', title: 'Bypass permissions', subtitle: 'Runs every tool without asking' },
  { value: 'ask', title: 'Asks permission', subtitle: 'Asks before it edits or runs anything' },
];
