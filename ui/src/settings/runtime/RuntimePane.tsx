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
  VERBOSITY_OPTIONS,
  type Found,
  type RuntimeSettings,
  type RuntimeStatus,
  type SettingsPatch,
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
      </section>

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
