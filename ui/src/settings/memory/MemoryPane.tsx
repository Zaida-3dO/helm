// Settings → Memory: what Helm indexes so every agent's search can find it. One switch turns the
// feature on; the list below names the sources (notes folders, chat logs, this PC's chats), each
// with who it belongs to, who may find it, and what the indexer last did with it. Adding a source
// or taking one off uploads or deletes that material on the hub, which is why it only happens from
// this page (or an agent's request with your Allow). Opening the page asks nothing of a server.

import { useCallback, useEffect, useMemo, useState } from 'react';
import { profilesOps } from '../../api/profiles.ts';
import { useShell } from '../../shell/context.ts';
import { Button, Checkbox, Picker, SettingRow, TextArea, TextInput } from '../../ui/index.ts';
import { memindexOps } from './api.ts';
import {
  addPayload,
  docsText,
  draftProblem,
  globsProblem,
  hubNotice,
  kindHasPath,
  KIND_TITLES,
  modelProblem,
  newDraft,
  type MemindexScope,
  nightlyProblem,
  parseGlobs,
  pendingText,
  scanText,
  SCOPE_OPTIONS,
  settingsChanges,
  type MemindexKind,
  type MemindexState,
  type MemindexSource,
  type SourceDraft,
} from './model.ts';
import './memory.css';

function errorText(e: unknown): string {
  return String((e as { message?: unknown })?.message ?? e);
}

interface OllamaServer {
  id: string;
  name: string;
}

export function MemoryPane() {
  const shell = useShell();
  const ops = useMemo(() => memindexOps(shell.api), [shell.api]);
  const [state, setState] = useState<MemindexState | null>(null);
  const [drafts, setDrafts] = useState<SourceDraft[]>([]);
  const [servers, setServers] = useState<OllamaServer[]>([]);
  const [people, setPeople] = useState<{ id: string; name: string }[]>([]);
  const [busy, setBusy] = useState(false);
  const [note, setNote] = useState<string | null>(null);
  const [error, setError] = useState<string | null>(null);

  useEffect(() => {
    let live = true;
    const show = (s: MemindexState) => live && setState(s);
    void ops
      .state()
      .then(show)
      .catch((e: unknown) => live && setError(errorText(e)));
    // The indexer's thread, the tool and other windows all change this view without asking the pane.
    const off = shell.bus.on('memindex', () => void ops.state().then(show).catch(() => undefined));
    void shell
      .api<{ servers: { id: string; name: string; kind: string; enabled: boolean }[] }>('local_models_state', {})
      .then((l) => live && setServers(l.servers.filter((s) => s.kind === 'ollama' && s.enabled).map((s) => ({ id: s.id, name: s.name }))))
      .catch(() => undefined);
    void profilesOps(shell.api)
      .list()
      .then((p) => live && setPeople(p.profiles.map((x) => ({ id: x.id, name: x.name }))))
      .catch(() => undefined);
    return () => {
      live = false;
      off();
    };
  }, [shell, ops]);

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

  const saveSettings = (patch: Parameters<typeof settingsChanges>[0]) =>
    run(async () => {
      setState(await ops.saveSettings(settingsChanges(patch)));
    });

  const changeSource = (id: string, patch: { enabled?: boolean; path?: string; personId?: string; scope?: MemindexScope; include?: string[]; exclude?: string[] }) =>
    run(async () => {
      setState(await ops.saveSource({ id, ...patch }));
    });

  const remove = (id: string) =>
    run(async () => {
      setState(await ops.removeSource(id));
      setNote('Source removed: its docs are off the hub.');
    });

  const addDraft = (kind: MemindexKind) => setDrafts((l) => [...l, newDraft(kind)]);

  const patchDraft = (key: string, patch: Partial<SourceDraft>) => setDrafts((l) => l.map((d) => (d.key === key ? { ...d, ...patch } : d)));

  const addSource = (d: SourceDraft) =>
    run(async () => {
      setState(await ops.saveSource(addPayload(d)));
      setDrafts((l) => l.filter((x) => x.key !== d.key));
      setNote('Source added: the first scan starts.');
    });

  /** The one-kind sources, added with a single click when this PC does not have them yet. */
  const singleKinds: MemindexKind[] = ['helm_chats', 'claude_code'];
  const missingSingle = singleKinds.filter((k) => !state?.sources.some((s) => s.kind === k));

  const now = Date.now();
  const hub = state ? hubNotice(state) : null;
  const pending = state ? pendingText(state.pending) : null;
  const personOptions = [{ value: '', title: 'This computer’s person' }, ...people.map((p) => ({ value: p.id, title: p.name }))];

  return (
    <div className="mem-pane" data-testid="settings-memory">
      <section className="mem-section">
        <div className="mem-section-head">
          <h3>Memory search</h3>
          <Button variant="secondary" data-testid="memory-index-now" disabled={!state || busy} title="Look over every source now, instead of waiting for a change or the nightly pass" onClick={() => void run(async () => { await ops.reindex({}); setNote('Indexing started: each source keeps working in the background.'); })}>
            Index now
          </Button>
        </div>
        <p className="mem-help">
          Helm can index your notes, saved chat logs and past conversations so every agent can search them by meaning as well as by words, from any computer. The text goes to the hub; embedding — turning it into meaning — happens on this
          computer while its model server is free, and new material is still searchable by words until it is embedded.
        </p>
        <SettingRow testId="memory-switch-row" title="Index this material for search" description="Off stops the watcher and the nightly pass; the hub keeps answering searches with what it already has.">
          <Checkbox label="On" checked={state?.enabled ?? false} data-testid="memory-switch" disabled={!state || busy} onChange={(on) => void saveSettings({ enabled: on })} />
        </SettingRow>
        {hub && (
          <div className={`mem-hub mem-hub-${hub.tone}`} role="status" data-testid="memory-hub" data-hub={state?.hub}>
            {hub.text}
          </div>
        )}
        {state?.status && (
          <div className="mem-status" data-testid="memory-status">
            {state.status}
          </div>
        )}
        {pending && (
          <div className="mem-pending" data-testid="memory-pending">
            {pending}
          </div>
        )}
      </section>

      <section className="mem-section">
        <h3>Embedding</h3>
        <p className="mem-help">
          Turning text into meaning takes the model below from your local model servers. While the graphics card is reserved or a local chat is mid-turn, Helm holds the embedding and keeps pushing the words, so nothing is lost — it is only found by words a little longer.
        </p>
        <SettingRow testId="memory-server-row" title="Model server for embedding" description="An Ollama server from Settings → Local models. None means search only ever matches words.">
          <Picker
            settings
            label="Model server for embedding"
            testId="memory-server-picker"
            value={state ? state.serverId : null}
            options={[{ value: '', title: 'None' }, ...servers.map((s) => ({ value: s.id, title: s.name }))]}
            disabled={!state || busy}
            onChange={(v: string) => void saveSettings({ serverId: v })}
          />
        </SettingRow>
        <CommitRow
          testId="memory-model"
          title="Embedding model"
          description="The model the server embeds with. Its name as the server lists it; the tag is left off."
          placeholder="nomic-embed-text"
          value={state?.model ?? ''}
          disabled={!state || busy}
          problem={modelProblem}
          onCommit={(v) => void saveSettings({ model: v })}
        />
        <CommitRow
          testId="memory-nightly"
          title="Nightly full check"
          description="Every day at this time Helm re-checks every source for changes the watcher missed."
          placeholder="03:30"
          value={state?.nightly ?? ''}
          disabled={!state || busy}
          problem={nightlyProblem}
          onCommit={(v) => void saveSettings({ nightly: v })}
        />
      </section>

      <section className="mem-section">
        <h3>Sources</h3>
        <p className="mem-help">What gets indexed. A source belongs to a person: their own searches rank their material first; shared material shows to everyone, ranked below their own.</p>
        {(state?.sources ?? []).length === 0 && drafts.length === 0 && <p className="mem-help">Nothing is indexed yet. Add a source below.</p>}
        {(state?.sources ?? []).map((s) => (
          <SourceCard
            key={s.id}
            source={s}
            now={now}
            busy={busy}
            personOptions={personOptions}
            onChange={(patch) => void changeSource(s.id, patch)}
            onRemove={() => void remove(s.id)}
          />
        ))}
        {drafts.map((d) => (
          <DraftCard key={d.key} draft={d} busy={busy} personOptions={personOptions} onPatch={(patch) => patchDraft(d.key, patch)} onAdd={() => void addSource(d)} onDiscard={() => setDrafts((l) => l.filter((x) => x.key !== d.key))} />
        ))}
        <div className="mem-actions">
          <Button variant="secondary" data-testid="memory-add-folder" disabled={!state || busy} onClick={() => addDraft('folder')}>
            Add a notes folder
          </Button>
          <Button variant="secondary" data-testid="memory-add-logs" disabled={!state || busy} onClick={() => addDraft('chat_logs')}>
            Add chat logs
          </Button>
          {missingSingle.map((k) => (
            <Button key={k} variant="secondary" data-testid={`memory-add-${k}`} disabled={!state || busy} onClick={() => void run(async () => { setState(await ops.saveSource({ id: null, kind: k })); setNote('Added: the first scan starts.'); })}>
              Add {KIND_TITLES[k].toLowerCase()}
            </Button>
          ))}
        </div>
      </section>

      {note && (
        <div className="setting-desc" role="status" data-testid="memory-note">
          {note}
        </div>
      )}
      {error && (
        <div className="ui-field-error" role="alert" data-testid="memory-error">
          {error}
        </div>
      )}
    </div>
  );
}

/** One registered source: what it reads, who it belongs to, and what the indexer says about it. */
function SourceCard({
  source,
  now,
  busy,
  personOptions,
  onChange,
  onRemove,
}: {
  source: MemindexSource;
  now: number;
  busy: boolean;
  personOptions: { value: string; title: string }[];
  onChange: (patch: { enabled?: boolean; path?: string; personId?: string; scope?: MemindexScope; include?: string[]; exclude?: string[] }) => void;
  onRemove: () => void;
}) {
  const hasPath = kindHasPath(source.kind);
  return (
    <div className="mem-source" data-testid={`memory-source-${source.id}`} data-source-id={source.id} data-kind={source.kind}>
      <div className="mem-source-head">
        <span className="mem-kind" data-testid="memory-kind">
          {KIND_TITLES[source.kind]}
        </span>
        {hasPath && <span className="mem-path" title={source.path}>{source.path}</span>}
        <span className="mem-spacer" />
        <Checkbox label="Enabled" checked={source.enabled} data-testid="memory-enabled" disabled={busy} onChange={(on) => onChange({ enabled: on })} />
        <Button variant="text" danger data-testid="memory-remove" disabled={busy} onClick={onRemove}>
          Remove
        </Button>
      </div>
      <div className="mem-source-fields">
        <label className="mem-field">
          <span className="mem-label">Owner</span>
          <Picker
            settings
            label="Owner"
            testId="memory-person"
            value={source.personId}
            options={personOptions}
            disabled={busy}
            onChange={(v: string) => onChange({ personId: v })}
          />
        </label>
        <label className="mem-field">
          <span className="mem-label">Who may find it</span>
          <Picker
            settings
            label="Who may find it"
            testId="memory-scope"
            value={source.scope}
            options={SCOPE_OPTIONS}
            disabled={busy}
            onChange={(v: string) => onChange({ scope: v as MemindexScope })}
          />
        </label>
      </div>
      {hasPath && (
        <>
          <GlobsArea kindLabel="Excluded files" testId="memory-exclude" label="Exclude" placeholder="one pattern per line, such as Private/**" value={source.exclude} busy={busy} onCommit={(globs) => onChange({ exclude: globs })} />
          <GlobsArea kindLabel="Included files" testId="memory-include" label="Include" placeholder="empty means every markdown file" value={source.include} busy={busy} onCommit={(globs) => onChange({ include: globs })} />
        </>
      )}
      <div className="mem-source-foot">
        <span className="mem-source-status" data-testid="memory-source-status">
          {source.status}
        </span>
        <span className="mem-scan" data-testid="memory-scan" title={`${docsText(source.docs, source.pushed)} · ${scanText(source.lastScan, now)}`}>
          {docsText(source.docs, source.pushed)} · {scanText(source.lastScan, now)}
        </span>
      </div>
    </div>
  );
}

/** A source being added: its folder, owner and sharing filled in before the first save. */
function DraftCard({
  draft,
  busy,
  personOptions,
  onPatch,
  onAdd,
  onDiscard,
}: {
  draft: SourceDraft;
  busy: boolean;
  personOptions: { value: string; title: string }[];
  onPatch: (patch: Partial<SourceDraft>) => void;
  onAdd: () => void;
  onDiscard: () => void;
}) {
  const hasPath = draft.kind !== null && kindHasPath(draft.kind);
  const problem = draftProblem(draft);
  return (
    <div className="mem-source mem-draft" data-testid={`memory-draft-${draft.key}`}>
      <div className="mem-source-head">
        <span className="mem-kind" data-testid="memory-kind">
          {draft.kind ? KIND_TITLES[draft.kind] : 'New source'}
        </span>
        <span className="mem-spacer" />
        <Button variant="text" data-testid="memory-discard" disabled={busy} onClick={onDiscard}>
          Discard
        </Button>
        <Button variant="primary" data-testid="memory-add" disabled={busy || problem !== null} title={problem ?? undefined} onClick={onAdd}>
          Add source
        </Button>
      </div>
      {hasPath && (
        <div className="mem-field">
          <label>
            <span className="mem-label">Folder</span>
            <TextInput
              className="mem-path-input"
              aria-label="Folder"
              spellCheck={false}
              placeholder="C:\Notes or \\server\share\notes"
              data-testid="memory-path"
              value={draft.path}
              onChange={(e) => onPatch({ path: e.target.value })}
            />
          </label>
        </div>
      )}
      <div className="mem-source-fields">
        <label className="mem-field">
          <span className="mem-label">Owner</span>
          <Picker settings label="Owner" testId="memory-person" value={draft.personId} options={personOptions} disabled={busy} onChange={(v: string) => onPatch({ personId: v })} />
        </label>
        <label className="mem-field">
          <span className="mem-label">Who may find it</span>
          <Picker settings label="Who may find it" testId="memory-scope" value={draft.scope} options={SCOPE_OPTIONS} disabled={busy} onChange={(v: string) => onPatch({ scope: v as typeof draft.scope })} />
        </label>
      </div>
      {hasPath && <GlobsArea kindLabel="Excluded files" testId="memory-exclude" label="Exclude" placeholder="one pattern per line, such as Private/**" value={draft.exclude} busy={busy} onCommit={(globs) => onPatch({ exclude: globs })} />}
      {problem && (
        <div className="ui-field-error" role="alert" data-testid="memory-draft-problem">
          {problem}
        </div>
      )}
    </div>
  );
}

/** One line per glob pattern, kept as a list; saved when you leave the box or press Enter's row out of it. */
function GlobsArea({ testId, label, placeholder, value, busy, onCommit }: { kindLabel: string; testId: string; label: string; placeholder: string; value: string[]; busy: boolean; onCommit: (globs: string[]) => void }) {
  const [text, setText] = useState<string | null>(null);
  const shown = text ?? value.join('\n');
  const problem = text === null ? null : globsProblem(parseGlobs(text));
  const commit = () => {
    if (text === null || problem) return;
    const globs = parseGlobs(text);
    setText(null);
    if (globs.join('\n') !== value.join('\n')) onCommit(globs);
  };
  return (
    <div className="mem-field">
      <label>
        <span className="mem-label">{label}</span>
        <TextArea
          className="mem-globs"
          rows={2}
          spellCheck={false}
          placeholder={placeholder}
          aria-label={label}
          data-testid={testId}
          disabled={busy}
          value={shown}
          onChange={(e) => setText(e.target.value)}
          onBlur={commit}
        />
      </label>
      {problem && (
        <div className="ui-field-error" role="alert" data-testid={`${testId}-problem`}>
          {problem}
        </div>
      )}
    </div>
  );
}

/** A text setting committed on blur or Enter; a problem stops the commit. */
function CommitRow({ testId, title, description, placeholder, value, disabled, problem, onCommit }: { testId: string; title: string; description: string; placeholder: string; value: string; disabled: boolean; problem: (v: string) => string | null; onCommit: (v: string) => void }) {
  const [text, setText] = useState<string | null>(null);
  const bad = text === null ? null : problem(text);
  const commit = () => {
    if (text === null || bad) return;
    const v = text.trim();
    setText(null);
    if (v !== value) onCommit(v);
  };
  return (
    <SettingRow testId={testId} title={title} description={description}>
      <TextInput
        className="mem-wide"
        spellCheck={false}
        placeholder={placeholder}
        aria-label={title}
        data-testid={`${testId}-input`}
        disabled={disabled}
        value={text ?? value}
        onChange={(e) => setText(e.target.value)}
        onBlur={commit}
        onKeyDown={(e) => {
          if (e.key === 'Enter') commit();
          if (e.key === 'Escape') setText(null);
        }}
      />
      {bad && (
        <div className="ui-field-error" role="alert" data-testid={`${testId}-problem`}>
          {bad}
        </div>
      )}
    </SettingRow>
  );
}
