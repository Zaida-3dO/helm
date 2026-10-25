// Settings → Runtime: whether Node and Claude Code are set up, how Claude
// Code is launched, and what new chats start with. Each control saves at once
// (`runtime_settings_save`); the executable saves with its button.

import { useCallback, useEffect, useState } from 'react';
import { useShell } from '../../shell/context.ts';
import { Button, Checkbox, Picker, SettingRow, TextInput, toast } from '../../ui/index.ts';
import {
  DEFAULT_MODEL,
  EFFORT_OPTIONS,
  modelOptions,
  PERMISSION_OPTIONS,
  statusLine,
  SUBAGENT_SCOPE_OPTIONS,
  turnOptions,
  VERBOSITY_OPTIONS,
  repoSummary,
  type Found,
  type RepoRow,
  type RuntimeSettings,
  type RuntimeStatus,
  type SettingsPatch,
  type SubagentScope,
} from './model.ts';
import './runtime.css';

function Component({ name, found, testId }: { name: string; found: Found | null; testId: string }) {
  const line = found ? statusLine(found) : null;
  return (
    <div className="rt-component" data-testid={testId} data-ready={line ? String(line.ready) : undefined}>
      <span className={`rt-dot${line ? (line.ready ? ' rt-dot-ok' : ' rt-dot-bad') : ''}`} aria-hidden="true" />
      <div className="rt-component-text">
        <div className="rt-component-name">
          {name}
          <span className="rt-component-state">{line ? (line.ready ? 'Ready' : 'Setup needed') : 'Checking…'}</span>
        </div>
        {line && <div className={line.ready ? 'rt-component-line' : 'rt-component-line rt-problem'}>{line.text}</div>}
        {found?.path && <div className="rt-component-path">{found.path}</div>}
      </div>
    </div>
  );
}

export function RuntimePane() {
  const shell = useShell();
  const [status, setStatus] = useState<RuntimeStatus | null>(null);
  const [checking, setChecking] = useState(false);
  const [settings, setSettings] = useState<RuntimeSettings | null>(null);
  const [models, setModels] = useState<{ id: string; name: string; description?: string }[]>([]);
  const [exe, setExe] = useState('');
  const [repos, setRepos] = useState<RepoRow[] | null>(null);

  const check = useCallback(() => {
    setChecking(true);
    void shell
      .api<RuntimeStatus>('runtime_status', {})
      .then(setStatus)
      .catch((e: unknown) => toast.show({ kind: 'error', text: `Could not check the runtime: ${String((e as Error).message ?? e)}` }))
      .finally(() => setChecking(false));
  }, [shell]);

  useEffect(() => {
    check();
    void shell
      .api<RuntimeSettings>('runtime_settings_get', {})
      .then((s) => {
        setSettings(s);
        setExe(s.executable);
      })
      .catch(() => undefined);
    void shell
      .api<{ entries: RepoRow[] }>('repo_list', {})
      .then((r) => setRepos(r.entries))
      .catch(() => setRepos([]));
    void shell
      .api<{ models: { id: string; name: string; description?: string }[] }>('model_catalogue', { harness: 'claude' })
      .then((c) => setModels(c.models))
      .catch(() => undefined);
  }, [shell, check]);

  const save = (patch: SettingsPatch, after?: () => void) => {
    const before = settings;
    if (settings) setSettings({ ...settings, ...patch, model: patch.model === undefined ? settings.model : patch.model || null });
    void shell
      .api<RuntimeSettings>('runtime_settings_save', patch)
      .then((s) => {
        setSettings(s);
        after?.();
      })
      .catch((e: unknown) => {
        setSettings(before);
        toast.show({ kind: 'error', text: `Could not save: ${String((e as Error).message ?? e)}` });
      });
  };

  const setWorktrees = (repo: RepoRow, allowed: boolean) => {
    setRepos((list) => list && list.map((r) => (r.repoId === repo.repoId ? { ...r, worktreesAllowed: allowed } : r)));
    void shell.api<RepoRow>('repo_set_worktrees', { repoId: repo.repoId, allowed }).catch((e: unknown) => {
      setRepos((list) => list && list.map((r) => (r.repoId === repo.repoId ? { ...r, worktreesAllowed: !allowed } : r)));
      toast.show({ kind: 'error', text: `Could not save: ${String((e as Error).message ?? e)}` });
    });
  };

  const s = settings;
  return (
    <div className="rt-pane" data-testid="settings-runtime">
      <section className="rt-section">
        <div className="rt-section-head">
          <h3>Setup</h3>
          <Button variant="secondary" onClick={check} disabled={checking} data-testid="runtime-recheck">
            {checking ? 'Checking…' : 'Check again'}
          </Button>
        </div>
        <div className="rt-components">
          <Component name="Node.js" found={status?.node ?? null} testId="runtime-node" />
          <Component name="Claude Code" found={status?.claude ?? null} testId="runtime-claude" />
        </div>
        {s?.problem && (
          <p className="rt-problem" data-testid="runtime-file-problem">
            {s.problem}. The defaults apply until you save a setting below.
          </p>
        )}
      </section>

      <section className="rt-section">
        <h3>Claude Code</h3>
        <p className="rt-help">These apply when a chat's worker next starts; a chat that is working keeps what it started with.</p>
        <SettingRow title="Claude Code executable" description="Leave empty and Helm finds it (claude on PATH, ~/.local/bin, or the VS Code extension). A wrapper script such as claude.cmd is refused.">
          <TextInput
            className="rt-exe"
            value={exe}
            placeholder="Found automatically"
            onChange={(e) => setExe(e.target.value)}
            data-testid="runtime-executable"
            aria-label="Claude Code executable"
          />
          <Button
            variant="secondary"
            disabled={!s || exe.trim() === s.executable}
            onClick={() => save({ executable: exe.trim() }, check)}
            data-testid="runtime-executable-save"
          >
            Save
          </Button>
        </SettingRow>
        <SettingRow title="Permission mode" description="How a chat's worker starts. Each chat can still switch mode from the composer.">
          <Picker
            settings
            label="Permission mode"
            testId="runtime-permission"
            value={s ? (s.skipPermissions ? 'bypass' : 'ask') : null}
            options={PERMISSION_OPTIONS}
            disabled={!s}
            onChange={(v: string) => save({ skipPermissions: v === 'bypass' })}
          />
        </SettingRow>
        <SettingRow title="Fast mode" description="Quicker replies on models that support it, at a higher token price.">
          <Checkbox checked={s?.fastMode ?? true} disabled={!s} onChange={(on) => save({ fastMode: on })} data-testid="runtime-fast-mode" aria-label="Fast mode" />
        </SettingRow>
        <SettingRow
          title="Use Claude Code's worktree tools"
          description="Lets Claude Code enter and leave its own worktrees. Off by default, and it stays off while any repository below has worktrees switched off."
        >
          <Checkbox
            checked={s?.worktreeTools ?? false}
            disabled={!s}
            onChange={(on) => save({ worktreeTools: on })}
            data-testid="runtime-worktree-tools"
            aria-label="Use Claude Code's worktree tools"
          />
        </SettingRow>
        <SettingRow title="Sub-agent limits" description="A sub-agent that has used its tool calls, or about 300k tokens of context, is told to write a short hand-off and stop. Your chat's own agent is never limited.">
          <Picker
            settings
            label="Sub-agent limits"
            testId="runtime-subagent-limits"
            value={s?.subagentLimits ?? null}
            options={SUBAGENT_SCOPE_OPTIONS}
            disabled={!s}
            onChange={(v: string) => save({ subagentLimits: v as SubagentScope })}
          />
          <Picker
            settings
            label="Sub-agent tool calls"
            testId="runtime-subagent-turns"
            value={s ? String(s.subagentSoftTurns) : null}
            options={turnOptions(s?.subagentSoftTurns ?? null)}
            disabled={!s || s.subagentLimits === 'off'}
            onChange={(v: string) => save({ subagentSoftTurns: Number(v) })}
          />
        </SettingRow>
        <SettingRow title="Refuse tool calls past the limit" description="Off: a sub-agent past its limit is only reminded to hand off. On: its tool calls are refused from 30% over, so it has to reply.">
          <Checkbox checked={s?.subagentHardStop ?? false} disabled={!s || s.subagentLimits === 'off'} onChange={(on) => save({ subagentHardStop: on })} data-testid="runtime-subagent-hard-stop" aria-label="Refuse tool calls past the limit" />
        </SettingRow>
        <SettingRow title="Cut long tool output" description="The output of Helm's own tools past about 5,000 characters is cut to its start and end, with the whole text saved to a file the agent can read.">
          <Checkbox checked={s?.toolOutputCap ?? true} disabled={!s} onChange={(on) => save({ toolOutputCap: on })} data-testid="runtime-output-cap" aria-label="Cut long tool output" />
        </SettingRow>
        <SettingRow
          title="Offer Claude Code's schedule tools"
          description="Claude Code's own cron, wake-up and remote-trigger tools. They never run: Helm turns each call down and points the chat at Helm's schedules, where you can see, pause and cancel them. Off by default."
        >
          <Checkbox
            checked={s?.scheduleTools ?? false}
            disabled={!s}
            onChange={(on) => save({ scheduleTools: on })}
            data-testid="runtime-schedule-tools"
            aria-label="Offer Claude Code's schedule tools"
          />
        </SettingRow>
      </section>

      {repos && repos.length > 0 && (
        <section className="rt-section" data-testid="runtime-repos">
          <h3>Repositories</h3>
          <p className="rt-help">
            Helm lists the git clones of your projects. Switch worktrees off for a repository and chats are stopped from running git worktree add there. This is a guard against accidents, not a lock: a program that does not use git.exe, or git called by its full path, is not stopped. Chats pick it up when they next start.
          </p>
          {repos.map((r) => (
            <SettingRow key={r.repoId} title={r.repoId} description={repoSummary(r)}>
              <Checkbox
                checked={r.worktreesAllowed}
                onChange={(on) => setWorktrees(r, on)}
                data-testid={`runtime-repo-worktrees-${r.repoId}`}
                aria-label={`Agents may create worktrees in ${r.repoId}`}
              />
            </SettingRow>
          ))}
        </section>
      )}

      <section className="rt-section">
        <h3>New chats</h3>
        <SettingRow title="Model" description="The model a new chat starts with. Change it for one chat from its composer.">
          <Picker
            settings
            label="Model for new chats"
            testId="runtime-model"
            value={s ? (s.model ?? DEFAULT_MODEL) : null}
            options={modelOptions(models, s?.model ?? null)}
            disabled={!s}
            onChange={(v: string) => save({ model: v === DEFAULT_MODEL ? '' : v })}
          />
        </SettingRow>
        <SettingRow title="Effort" description="How hard Claude thinks before it answers. Higher is slower; Extra high is Claude Code's own default.">
          <Picker
            settings
            label="Effort for new chats"
            testId="runtime-effort"
            value={s?.effort ?? null}
            options={EFFORT_OPTIONS}
            disabled={!s}
            onChange={(v: string) => save({ effort: v })}
          />
        </SettingRow>
        <SettingRow title="Response length" description="How long the agent's replies are, in every chat.">
          <Picker
            settings
            label="Response length"
            testId="runtime-verbosity"
            value={s?.responseVerbosity ?? null}
            options={VERBOSITY_OPTIONS}
            disabled={!s}
            onChange={(v: string) => save({ responseVerbosity: v })}
          />
        </SettingRow>
      </section>
    </div>
  );
}
