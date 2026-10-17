// Settings, Actions: add, edit, hide, reorder and bind the one-click buttons.
// Saves are compare-and-set against the revision the pane loaded; a stale save reloads.

import { useCallback, useEffect, useMemo, useState } from 'react';
import { actionOps, type ActionButton } from '../../api/actions.ts';
import { ApiError, type Api } from '../../api/client.ts';
import { notifyActionsChanged } from '../../actions/changes.ts';
import { addButton, firstProblem, moveButton, newButton, removeButton, updateButton, MAX_BUTTONS } from '../../actions/model.ts';
import { Button, Checkbox, Segmented, TextArea, TextInput } from '../../ui/index.ts';
import '../../actions/actions.css';

interface ProjectOption {
  id: string;
  name: string;
  path?: string | null;
}

export function ActionsPane({ api }: { api: Api }) {
  const ops = useMemo(() => actionOps(api), [api]);
  const [revision, setRevision] = useState(0);
  const [buttons, setButtons] = useState<ActionButton[]>([]);
  const [saved, setSaved] = useState<ActionButton[]>([]);
  const [projects, setProjects] = useState<ProjectOption[]>([]);
  const [status, setStatus] = useState<string | null>(null);
  const [loaded, setLoaded] = useState(false);

  const load = useCallback(async () => {
    const set = await ops.get();
    setRevision(set.revision);
    setButtons(set.buttons);
    setSaved(set.buttons);
    setLoaded(true);
  }, [ops]);

  useEffect(() => {
    load().catch((e: unknown) => setStatus(`Could not load the buttons: ${String((e as Error).message ?? e)}`));
    // The project list belongs to the sidebar; without it a new-chat button cannot be bound.
    api<ProjectOption[]>('project_list', {})
      .then((p) => setProjects(p.filter((x) => x.path)))
      .catch(() => setProjects([]));
  }, [api, load]);

  const problem = firstProblem(buttons);
  const dirty = JSON.stringify(buttons) !== JSON.stringify(saved);

  const save = async () => {
    if (problem) return;
    setStatus('Saving…');
    try {
      const set = await ops.save(revision, buttons);
      setRevision(set.revision);
      setButtons(set.buttons);
      setSaved(set.buttons);
      setStatus('Saved.');
      notifyActionsChanged();
    } catch (e) {
      if (e instanceof ApiError && e.code === 'conflict') {
        await load();
        setStatus('The buttons changed elsewhere, so they were reloaded. Make your edit again.');
      } else {
        setStatus(`Could not save: ${String((e as Error).message ?? e)}`);
      }
    }
  };

  if (!loaded) return <div className="actions-pane muted">{status ?? 'Loading…'}</div>;

  return (
    <div className="actions-pane" data-testid="settings-pane" data-pane="actions">
      <h2>Actions</h2>
      <p className="muted">
        One-click buttons that send a message for you. They never touch your draft, and Helm runs nothing itself: a button is
        only text the agent reads. Buttons marked &quot;New chat&quot; start a chat in a project you bind them to.
      </p>
      {buttons.map((b, i) => {
        const bad = problem?.id === b.id ? problem.message : null;
        return (
          <div key={b.id} className={`action-edit${b.hidden ? ' is-hidden' : ''}`} data-testid="action-edit" data-action={b.seedId ?? b.id}>
            <div className="action-edit-row">
              <div style={{ flex: 1, minWidth: 160 }}>
                <label htmlFor={`label-${b.id}`}>Label</label>
                <TextInput
                  id={`label-${b.id}`}
                  type="text"
                  value={b.label}
                  maxLength={80}
                  onChange={(e) => setButtons((l) => updateButton(l, b.id, { label: e.target.value }))}
                />
              </div>
              <Segmented
                label="Sends"
                value={b.kind}
                options={[
                  { value: 'prompt', label: 'Into this chat' },
                  { value: 'new_chat', label: 'Into a new chat' },
                ]}
                onChange={(kind) => setButtons((l) => updateButton(l, b.id, { kind }))}
              />
              <Segmented
                label="Shown"
                value={b.placement}
                options={[
                  { value: 'topbar', label: 'In the top bar' },
                  { value: 'overflow', label: 'In the more menu' },
                ]}
                onChange={(placement) => setButtons((l) => updateButton(l, b.id, { placement }))}
              />
            </div>
            {b.kind === 'new_chat' && (
              <div>
                <label htmlFor={`project-${b.id}`}>Project</label>
                <select
                  id={`project-${b.id}`}
                  value={b.projectId ?? ''}
                  onChange={(e) => setButtons((l) => updateButton(l, b.id, { projectId: e.target.value || null }))}
                >
                  <option value="">Not bound yet</option>
                  {projects.map((p) => (
                    <option key={p.id} value={p.id}>
                      {p.name}
                    </option>
                  ))}
                  {b.projectId && !projects.some((p) => p.id === b.projectId) && <option value={b.projectId}>(unavailable project)</option>}
                </select>
              </div>
            )}
            <div>
              <label htmlFor={`text-${b.id}`}>Message</label>
              <TextArea id={`text-${b.id}`} value={b.text} onChange={(e) => setButtons((l) => updateButton(l, b.id, { text: e.target.value }))} />
            </div>
            {bad && (
              <div className="error" role="alert">
                {bad}
              </div>
            )}
            <div className="action-edit-row">
              <Checkbox checked={b.hidden} onChange={(hidden) => setButtons((l) => updateButton(l, b.id, { hidden }))} label="Hidden" />
              <Button variant="text" disabled={i === 0} onClick={() => setButtons((l) => moveButton(l, b.id, -1))} label={`Move ${b.label || 'button'} earlier`}>
                Up
              </Button>
              <Button variant="text" disabled={i === buttons.length - 1} onClick={() => setButtons((l) => moveButton(l, b.id, 1))} label={`Move ${b.label || 'button'} later`}>
                Down
              </Button>
              <Button variant="text" danger onClick={() => setButtons((l) => removeButton(l, b.id))}>
                Delete
              </Button>
            </div>
          </div>
        );
      })}
      <div className="action-edit-row">
        <Button variant="text" data-testid="action-add" disabled={buttons.length >= MAX_BUTTONS} onClick={() => setButtons((l) => addButton(l, newButton(globalThis.crypto.randomUUID())))}>
          Add a button
        </Button>
        <Button variant="primary" data-testid="actions-save" disabled={!dirty || problem !== null} onClick={() => void save()}>
          Save
        </Button>
        <Button variant="text" disabled={!dirty} onClick={() => setButtons(saved)}>
          Discard changes
        </Button>
        {status && (
          <span className="muted" role="status">
            {status}
          </span>
        )}
      </div>
    </div>
  );
}
