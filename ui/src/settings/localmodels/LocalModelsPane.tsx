// Settings → Local models: the model servers on this machine (Strata, Ollama), their keys, and how
// chats on local models run. Helm reaches a server only when asked: this pane asks once when it
// opens, and again for Test. A key is write-only (set, replace, forget; it is never shown).
// Server fields are drafts until Save; the settings below the list save as they change.

import { useCallback, useEffect, useMemo, useRef, useState } from 'react';
import { useShell } from '../../shell/context.ts';
import { Button, Checkbox, Picker, SettingRow, TextInput } from '../../ui/index.ts';
import { localModelsOps, type SettingsPatch } from './api.ts';
import {
  clampTo,
  COMPARE_OPTIONS,
  draftProblem,
  newDraft,
  rangeText,
  reachText,
  sameDrafts,
  TOOLS_OPTIONS,
  toDraft,
  type CompareModel,
  type LocalModelsState,
  type Range,
  type ServerDraft,
  type TestResult,
  type ToolsSetup,
} from './model.ts';
import './localmodels.css';

function errorText(e: unknown): string {
  return String((e as { message?: unknown })?.message ?? e);
}

export function LocalModelsPane() {
  const shell = useShell();
  const ops = useMemo(() => localModelsOps(shell.api), [shell.api]);
  const [state, setState] = useState<LocalModelsState | null>(null);
  const [drafts, setDrafts] = useState<ServerDraft[]>([]);
  const [keyDrafts, setKeyDrafts] = useState<Record<string, string>>({});
  const [tests, setTests] = useState<Record<string, TestResult | 'running'>>({});
  const [checking, setChecking] = useState(false);
  const [busy, setBusy] = useState(false);
  const [note, setNote] = useState<string | null>(null);
  const [error, setError] = useState<string | null>(null);

  const baseline = useMemo(() => (state?.servers ?? []).map(toDraft), [state]);
  const dirty = !sameDrafts(drafts, baseline);
  const dirtyRef = useRef(false);
  dirtyRef.current = dirty;

  /** Takes in the host's state; the server cards follow it unless the user is in the middle of editing them. */
  const adopt = useCallback((s: LocalModelsState) => {
    setState(s);
    if (!dirtyRef.current) setDrafts(s.servers.map(toDraft));
  }, []);

  useEffect(() => {
    let live = true;
    // Show what Helm holds at once, then ask the servers once: nothing is asked before the pane opens.
    void ops
      .state()
      .then((s) => live && adopt(s))
      .catch((e: unknown) => live && setError(errorText(e)))
      .then(() => {
        if (!live) return;
        setChecking(true);
        return ops
          .refresh()
          .then(() => ops.state())
          .then((s) => live && adopt(s))
          .catch(() => undefined)
          .finally(() => live && setChecking(false));
      });
    // Other changes (an agent's settings tool, another window) refresh the view without asking the servers again.
    const off = shell.bus.on('local_models', () => void ops.state().then((s) => live && adopt(s)).catch(() => undefined));
    return () => {
      live = false;
      off();
    };
  }, [shell, ops, adopt]);

  /** Run one action: clear the old messages, show its error if it fails. */
  const run = async (action: () => Promise<void>) => {
    setBusy(true);
    setError(null);
    setNote(null);
    try {
      await action();
    } catch (e) {
      setError(errorText(e));
    } finally {
      setBusy(false);
    }
  };

  const patchDraft = (key: string, patch: Partial<ServerDraft>) => setDrafts((list) => list.map((d) => (d.key === key ? { ...d, ...patch } : d)));
  const problem = drafts.map(draftProblem).find((p) => p !== null) ?? null;

  const saveServers = () =>
    run(async () => {
      const s = await ops.saveServers(drafts);
      setState(s);
      setDrafts(s.servers.map(toDraft));
      setNote('Saved.');
    });

  const saveSettings = (patch: SettingsPatch) =>
    run(async () => {
      adopt(await ops.saveSettings(patch));
    });

  const setKey = (id: string) =>
    run(async () => {
      adopt(await ops.setKey(id, keyDrafts[id] ?? ''));
      setKeyDrafts((k) => ({ ...k, [id]: '' }));
      setNote('Key saved.');
    });

  const forgetKey = (id: string) =>
    run(async () => {
      adopt(await ops.forgetKey(id));
      setNote('Key forgotten.');
    });

  const test = async (id: string) => {
    setTests((t) => ({ ...t, [id]: 'running' }));
    try {
      const r = await ops.test(id);
      setTests((t) => ({ ...t, [id]: r }));
      void ops.state().then(adopt).catch(() => undefined);
    } catch (e) {
      setTests((t) => ({ ...t, [id]: { ok: false, message: errorText(e) } }));
    }
  };

  const remove = (key: string) => setDrafts((list) => list.filter((d) => d.key !== key));

  const s = state;
  return (
    <div className="lm-pane" data-testid="settings-localmodels">
      <section className="lm-section">
        <div className="lm-section-head">
          <h3>Servers</h3>
          {checking && (
            <span className="lm-checking" role="status" data-testid="localmodels-checking">
              Checking the servers…
            </span>
          )}
        </div>
        <p className="lm-help">
          Local models run on this machine, behind a model server. Helm only talks to a server when you ask: when this page opens, when you open the model picker, on Test, and when a local chat sends a message. A
          server must be on this machine (127.0.0.1, localhost or ::1) and use plain http.
        </p>
        {drafts.length === 0 && s && <p className="lm-help">No servers yet. Add one below.</p>}
        {drafts.map((d) => {
          const view = s?.servers.find((v) => v.id === d.id);
          const saved = baseline.find((b) => b.id === d.id);
          const edited = !saved || !sameDrafts([d], [saved]);
          const reach = reachText(view);
          const t = d.id ? tests[d.id] : undefined;
          const p = draftProblem(d);
          return (
            <div className="lm-server" key={d.key} data-testid={`localmodels-server-${d.id || d.key}`} data-server-id={d.id}>
              <div className="lm-server-head">
                <TextInput className="lm-name" aria-label="Server name" data-testid="localmodels-name" value={d.name} onChange={(e) => patchDraft(d.key, { name: e.target.value })} />
                <span className="lm-kind" data-testid="localmodels-kind">
                  {d.kind === 'strata' ? 'Strata' : 'Ollama'}
                </span>
                <Checkbox label="Enabled" checked={d.enabled} data-testid="localmodels-enabled" onChange={(on) => patchDraft(d.key, { enabled: on })} />
                <Button variant="text" danger data-testid="localmodels-remove" onClick={() => remove(d.key)}>
                  Remove
                </Button>
              </div>
              <div className="lm-field">
                <label>
                  <span className="lm-label">Address</span>
                  <TextInput
                    aria-label="Server address"
                    data-testid="localmodels-url"
                    spellCheck={false}
                    placeholder="http://127.0.0.1:8080"
                    value={d.baseUrl}
                    onChange={(e) => patchDraft(d.key, { baseUrl: e.target.value })}
                  />
                </label>
              </div>
              {d.kind === 'strata' && (
                <div className="lm-field">
                  <label>
                    <span className="lm-label">Model name</span>
                    <TextInput
                      aria-label="Model name"
                      data-testid="localmodels-model"
                      spellCheck={false}
                      placeholder="Used when the server does not list its models"
                      value={d.model}
                      onChange={(e) => patchDraft(d.key, { model: e.target.value })}
                    />
                  </label>
                </div>
              )}
              {(d.kind === 'strata' || view?.hasKey) && (
                <div className="lm-field lm-key">
                  <span className="lm-label">Key</span>
                  {d.id ? (
                    <>
                      <TextInput
                        type="password"
                        autoComplete="off"
                        spellCheck={false}
                        aria-label="Server key"
                        data-testid="localmodels-key-input"
                        placeholder={view?.hasKey ? 'Paste a new key to replace it' : 'Paste the key'}
                        value={keyDrafts[d.id] ?? ''}
                        onChange={(e) => setKeyDrafts((k) => ({ ...k, [d.id]: e.target.value }))}
                        onKeyDown={(e) => {
                          if (e.key === 'Enter' && (keyDrafts[d.id] ?? '').trim() && !busy) void setKey(d.id);
                        }}
                      />
                      <Button variant="primary" data-testid="localmodels-key-save" disabled={busy || !(keyDrafts[d.id] ?? '').trim()} onClick={() => void setKey(d.id)}>
                        {view?.hasKey ? 'Replace' : 'Save key'}
                      </Button>
                      {view?.hasKey && (
                        <Button variant="text" danger data-testid="localmodels-key-forget" disabled={busy} onClick={() => void forgetKey(d.id)}>
                          Forget
                        </Button>
                      )}
                      <span className="lm-key-state" data-testid="localmodels-key-state" data-has-key={view?.hasKey ?? false}>
                        {view?.hasKey ? 'A key is set. It is kept in the Windows credential store and never shown.' : 'No key is set.'}
                      </span>
                    </>
                  ) : (
                    <span className="lm-key-state">Save the server first, then add its key.</span>
                  )}
                </div>
              )}
              <div className="lm-server-foot">
                <span className={`lm-reach lm-reach-${reach.tone}`} data-testid="localmodels-reach" data-tone={d.id ? reach.tone : 'idle'}>
                  {d.id ? reach.text : 'Not saved yet'}
                </span>
                {t && t !== 'running' && (
                  <span className={`lm-result ${t.ok ? 'lm-result-ok' : 'lm-result-bad'}`} role="status" data-testid="localmodels-test-result" data-ok={t.ok}>
                    {t.message}
                  </span>
                )}
                <Button
                  variant="secondary"
                  data-testid="localmodels-test"
                  disabled={!d.id || edited || t === 'running'}
                  title={!d.id || edited ? 'Save your changes first: Test uses what is saved.' : 'Ask this server what it has'}
                  onClick={() => void test(d.id)}
                >
                  {t === 'running' ? 'Testing…' : 'Test'}
                </Button>
              </div>
              {p && edited && (
                <div className="ui-field-error" role="alert" data-testid="localmodels-draft-problem">
                  {p}
                </div>
              )}
            </div>
          );
        })}
        <div className="lm-actions">
          <Button variant="secondary" data-testid="localmodels-add-strata" onClick={() => setDrafts((l) => [...l, newDraft('strata')])}>
            Add a Strata server
          </Button>
          <Button variant="secondary" data-testid="localmodels-add-ollama" onClick={() => setDrafts((l) => [...l, newDraft('ollama')])}>
            Add an Ollama server
          </Button>
          <span className="lm-spacer" />
          {dirty && (
            <Button variant="text" data-testid="localmodels-discard" disabled={busy} onClick={() => setDrafts(baseline)}>
              Discard changes
            </Button>
          )}
          <Button variant="primary" data-testid="localmodels-save" disabled={busy || !dirty || problem !== null} title={problem ?? undefined} onClick={() => void saveServers()}>
            Save servers
          </Button>
        </div>
      </section>

      <section className="lm-section">
        <h3>Chats on local models</h3>
        <SettingRow
          testId="localmodels-tools"
          title="Tools in local chats"
          description="Lean gives a chat Helm's own tools and leaves your Claude Code MCP servers off, which a small local model copes with better. Full gives it everything a Claude chat has. A chat sees the change from its next start."
        >
          <Picker
            settings
            label="Tools in local chats"
            testId="localmodels-tools-picker"
            value={s?.tools ?? null}
            options={TOOLS_OPTIONS}
            disabled={!s || busy}
            onChange={(v: string) => void saveSettings({ tools: v as ToolsSetup })}
          />
        </SettingRow>
        <SettingRow
          testId="localmodels-compare"
          title="Compare local chats with"
          description="The cost dialog shows what the tokens of local chats would have cost on this Claude model, at API prices. It is an estimate for comparison; local chats cost nothing and never count toward your plan."
        >
          <Picker
            settings
            label="Compare local chats with"
            testId="localmodels-compare-picker"
            value={s?.compareWith ?? null}
            options={COMPARE_OPTIONS}
            disabled={!s || busy}
            onChange={(v: string) => void saveSettings({ compareWith: v as CompareModel })}
          />
        </SettingRow>
      </section>

      <section className="lm-section">
        <h3>Memory and context</h3>
        <NumberRow
          testId="localmodels-context"
          title="Ollama context size"
          unit="tokens"
          description="How much a chat on an Ollama model can hold. Claude Code cannot tell Ollama this itself, so Helm makes its own copy of each model with this context size (same weights, no extra disk) and chats use the copy. Ollama's own settings and its other users are not touched. A larger context uses more memory."
          value={s?.ollamaContext}
          range={s?.ollamaContextRange}
          busy={busy}
          onSave={(n) => void saveSettings({ ollamaContext: n })}
        />
        <NumberRow
          testId="localmodels-keepalive"
          title="Keep an Ollama model loaded"
          unit="minutes"
          description="How long Ollama keeps the model in memory after a turn. Helm sets this for its own turns only."
          value={s?.keepAliveMinutes}
          range={s?.keepAliveRange}
          busy={busy}
          onSave={(n) => void saveSettings({ keepAliveMinutes: n })}
        />
        <NumberRow
          testId="localmodels-strata-need"
          title="Memory a Strata model needs"
          unit="GB"
          description="Before a model that is not loaded starts a turn, Helm compares this with the free graphics memory. If it will not fit, it unloads idle models on the other servers first; if one of them is answering a chat, the turn waits and you are told."
          value={s?.strataNeedGb}
          range={s?.strataNeedRange}
          busy={busy}
          onSave={(n) => void saveSettings({ strataNeedGb: n })}
        />
      </section>

      {note && (
        <div className="setting-desc" role="status" data-testid="localmodels-note">
          {note}
        </div>
      )}
      {error && (
        <div className="ui-field-error" role="alert" data-testid="localmodels-error">
          {error}
        </div>
      )}
    </div>
  );
}

/** A number setting: typed, kept inside its range when committed (Enter or leaving the field), and saved. */
function NumberRow({
  testId,
  title,
  description,
  unit,
  value,
  range,
  busy,
  onSave,
}: {
  testId: string;
  title: string;
  description: string;
  unit: string;
  value: number | undefined;
  range: Range | undefined;
  busy: boolean;
  onSave: (n: number) => void;
}) {
  const [text, setText] = useState<string | null>(null);
  const shown = text ?? (value === undefined ? '' : String(value));
  const commit = () => {
    if (text === null || !range) return;
    const n = clampTo(range, text);
    setText(null);
    if (n !== null && n !== value) onSave(n);
  };
  return (
    <SettingRow testId={testId} title={title} description={`${description}${range ? ` ${rangeText(range)}.` : ''}`}>
      <TextInput
        className="lm-number"
        inputMode="numeric"
        aria-label={`${title} (${unit})`}
        data-testid={`${testId}-input`}
        disabled={value === undefined || busy}
        value={shown}
        onChange={(e) => setText(e.target.value)}
        onBlur={commit}
        onKeyDown={(e) => {
          if (e.key === 'Enter') commit();
          if (e.key === 'Escape') setText(null);
        }}
      />
      <span className="lm-unit">{unit}</span>
    </SettingRow>
  );
}
