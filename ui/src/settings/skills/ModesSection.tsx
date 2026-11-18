// Modes: Markdown files a layer offers that a chat can switch on (a short index line always shows;
// the body is read when the mode is on). A list with one inline editor. A save carries the
// revision the mode was read at; if the file changed since, the host refuses and the editor offers
// to load the newer one.

import { useCallback, useEffect, useMemo, useState } from 'react';
import { useShell } from '../../shell/context.ts';
import { Button, TextArea, TextInput, type ConfirmSpec } from '../../ui/index.ts';
import { modeOps, type ModeFile, type ModeOps } from '../../modes/api.ts';
import { draftOf, EMPTY_FORM, formOf, type ModeForm } from '../../modes/form.ts';
import { errorCode, errorText } from './common.ts';
import type { RunAction } from './RulesSection.tsx';

interface Shared {
  ops: ModeOps;
  layer: string;
  busy: boolean;
  run: RunAction;
  confirm: (spec: ConfirmSpec) => void;
}

const LISTS: { key: 'triggers' | 'mcp' | 'skills' | 'rules' | 'tools' | 'builtins'; label: string; hint: string }[] = [
  { key: 'triggers', label: 'Triggers', hint: 'words that suggest this mode, comma separated' },
  { key: 'mcp', label: 'MCP servers', hint: 'servers this mode brings in, by name' },
  { key: 'skills', label: 'Skills', hint: 'skills to point at' },
  { key: 'rules', label: 'Rules', hint: 'rules to point at' },
  { key: 'tools', label: 'Tools', hint: 'Helm tool groups it adds' },
  { key: 'builtins', label: 'Built-ins', hint: 'built-in Claude Code tools it adds' },
];

function ModeEditor({ ops, layer, busy, run, confirm, mode, onClose }: Shared & { mode: ModeFile | null; onClose: () => void }) {
  const [form, setForm] = useState<ModeForm>(mode ? formOf(mode) : EMPTY_FORM);
  const [revision, setRevision] = useState<string | undefined>(mode?.revision);
  const [error, setError] = useState<string | null>(null);
  const [stale, setStale] = useState(false);
  const set = (patch: Partial<ModeForm>) => setForm((f) => ({ ...f, ...patch }));

  const loadNewer = () =>
    ops
      .list(layer)
      .then((r) => {
        const m = r.modes.find((x) => x.name === mode?.name);
        if (!m) return onClose();
        setForm(formOf(m));
        setRevision(m.revision);
        setStale(false);
        setError(null);
      })
      .catch((e: unknown) => setError(errorText(e)));

  const save = () =>
    run(async () => {
      const name = form.name.trim();
      try {
        await ops.save(layer, name, draftOf(form), revision);
      } catch (e) {
        if (errorCode(e) === 'conflict') {
          setStale(true);
          setError(null);
          return;
        }
        throw e;
      }
      onClose();
      return mode ? `Saved ${name}.` : `Added ${name}.`;
    });

  const askDelete = () =>
    mode &&
    confirm({
      title: 'Delete the mode?',
      body: `${mode.name} is removed from this list. Chats that have it on keep it until they next start.`,
      confirm: 'Delete',
      danger: true,
      onConfirm: () =>
        run(async () => {
          await ops.remove(layer, mode.name);
          onClose();
          return `Deleted ${mode.name}.`;
        }),
    });

  return (
    <div className="kn-editor" data-testid="knowledge-mode-editor">
      {error && (
        <div className="ui-field-error" role="alert" data-testid="knowledge-mode-error">
          {error}
        </div>
      )}
      {stale && (
        <div className="ui-field-error" role="alert" data-testid="knowledge-mode-stale">
          This mode changed since you opened it, so nothing was saved.{' '}
          <Button variant="text" onClick={() => void loadNewer()}>
            Load the newer text
          </Button>
        </div>
      )}
      <TextInput
        aria-label="Mode name"
        placeholder="Name (lowercase letters, digits, dashes)"
        value={form.name}
        maxLength={64}
        disabled={mode !== null}
        onChange={(e) => set({ name: e.target.value })}
        data-testid="knowledge-mode-name"
      />
      <TextInput
        aria-label="Mode description"
        placeholder="One line for the index chats see"
        value={form.description}
        maxLength={200}
        onChange={(e) => set({ description: e.target.value })}
        data-testid="knowledge-mode-description"
      />
      {LISTS.map((l) => (
        <TextInput
          key={l.key}
          aria-label={l.label}
          placeholder={`${l.label}: ${l.hint}`}
          value={form[l.key]}
          onChange={(e) => set({ [l.key]: e.target.value })}
          data-testid={`knowledge-mode-${l.key}`}
        />
      ))}
      <TextInput
        aria-label="Model"
        placeholder="Model: the one this mode prefers, if any"
        value={form.model}
        onChange={(e) => set({ model: e.target.value })}
        data-testid="knowledge-mode-model"
      />
      <TextArea
        aria-label="Mode text"
        placeholder="What to know in this mode and where to look."
        rows={8}
        value={form.body}
        onChange={(e) => set({ body: e.target.value })}
        data-testid="knowledge-mode-body"
      />
      <div className="kn-actions">
        <Button variant="primary" disabled={busy || !form.name.trim()} data-testid="knowledge-mode-save" onClick={() => void save()}>
          Save
        </Button>
        <Button variant="secondary" disabled={busy} data-testid="knowledge-mode-cancel" onClick={onClose}>
          Cancel
        </Button>
        {mode && (
          <Button variant="text" danger disabled={busy} data-testid="knowledge-mode-delete" onClick={askDelete}>
            Delete
          </Button>
        )}
      </div>
    </div>
  );
}

export function ModesSection({ layer, busy, run, confirm }: Omit<Shared, 'ops'>) {
  const shell = useShell();
  const ops = useMemo(() => modeOps(shell.api), [shell.api]);
  const [modes, setModes] = useState<ModeFile[]>([]);
  const [loadError, setLoadError] = useState<string | null>(null);
  // `null`: nothing open; `'new'`: a blank editor; otherwise the name of the mode being edited.
  const [open, setOpen] = useState<string | null>(null);

  const load = useCallback(
    () =>
      ops
        .list(layer)
        .then((r) => {
          setModes(r.modes);
          setLoadError(null);
        })
        .catch((e: unknown) => setLoadError(errorText(e))),
    [ops, layer],
  );
  // A save or delete emits the knowledge event, which brings the list up to date.
  useEffect(() => {
    setOpen(null);
    setModes([]);
    void load();
    return shell.bus.on('knowledge-changed', () => void load());
  }, [shell, load]);

  const editing = open && open !== 'new' ? (modes.find((m) => m.name === open) ?? null) : null;
  const shared = { ops, layer, busy, run, confirm };

  return (
    <section className="kn-section" data-testid="knowledge-modes">
      <div className="kn-head">
        <h3 className="hub-heading">Modes</h3>
        <div className="kn-actions">
          <Button variant="primary" disabled={busy} data-testid="knowledge-mode-new" onClick={() => setOpen('new')}>
            New mode
          </Button>
        </div>
      </div>
      <div className="setting-desc">
        A mode is a topic a chat can switch on: what to know and where to look for it. Chats see a one-line index of the modes; the text is read when a mode is on. A project can start its chats with some already on.
      </div>
      {loadError && (
        <div className="ui-field-error" role="alert" data-testid="knowledge-modes-error">
          {loadError}
        </div>
      )}
      {modes.length === 0 && open !== 'new' && !loadError && (
        <div className="setting-desc" data-testid="knowledge-modes-empty">
          No modes in this list yet.
        </div>
      )}
      {modes.map((m) =>
        open === m.name ? (
          <ModeEditor key={m.name} {...shared} mode={editing} onClose={() => setOpen(null)} />
        ) : (
          <div className="skills-row" key={m.name} data-testid={`knowledge-modes-item-${m.name}`}>
            <div className="skills-what">
              <div className="skills-name">{m.name}</div>
              <div className="skills-desc">{m.description || 'No description'}</div>
            </div>
            <Button variant="text" disabled={busy} data-testid={`knowledge-mode-edit-${m.name}`} onClick={() => setOpen(m.name)}>
              Edit
            </Button>
          </div>
        ),
      )}
      {open === 'new' && <ModeEditor {...shared} mode={null} onClose={() => setOpen(null)} />}
    </section>
  );
}
