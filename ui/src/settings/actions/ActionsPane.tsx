// Settings, Actions: add, edit, hide, reorder and bind the one-click buttons.
// Saves are compare-and-set against the revision the pane loaded; a stale save reloads.
//
// The pane is a compact list, one row per button: its label, where it shows, where it sends and a
// one-line preview of its message. Clicking a row opens its editor underneath.

import { useCallback, useEffect, useMemo, useState } from 'react';
import { actionOps, type ActionButton } from '../../api/actions.ts';
import { ApiError, type Api } from '../../api/client.ts';
import { notifyActionsChanged } from '../../actions/changes.ts';
import { addButton, firstProblem, moveButton, newButton, removeButton, updateButton, MAX_BUTTONS } from '../../actions/model.ts';
import { Button, Segmented, TextArea, TextInput } from '../../ui/index.ts';
import { IconChevronDown, IconChevronRight, IconChevronUp, IconEye, IconEyeOff, IconTrash } from '../../ui/icons.tsx';
import { nameOf, previewLine, sendsTo, showsIn } from './summary.ts';
import '../../actions/actions.css';
import './actions-pane.css';

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
  /** The button whose editor is open (one at a time), and the one waiting on a delete confirm. */
  const [openId, setOpenId] = useState<string | null>(null);
  const [confirmId, setConfirmId] = useState<string | null>(null);

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

  const add = () => {
    const b = newButton(globalThis.crypto.randomUUID());
    setButtons((l) => addButton(l, b));
    setOpenId(b.id);
    setConfirmId(null);
    // The new editor is at the end of the list: focusing its label also scrolls it into view.
    requestAnimationFrame(() => document.getElementById(`label-${b.id}`)?.focus());
  };

  if (!loaded) return <div className="actions-pane muted">{status ?? 'Loading…'}</div>;

  return (
    <div className="actions-pane" data-testid="settings-pane" data-pane="actions">
      <h2>Actions</h2>
      <p className="muted">
        One-click buttons that send a message for you. They never touch your draft, and Helm runs nothing itself: a button is
        only text the agent reads. Buttons marked &quot;New chat&quot; start a chat in a project you bind them to.
      </p>
      {buttons.length === 0 ? (
        <div className="ap-empty">No buttons yet. Add one to get started.</div>
      ) : (
        <ul className="ap-list">
          {buttons.map((b, i) => {
            const name = nameOf(b);
            const open = openId === b.id;
            const bad = problem?.id === b.id ? problem.message : null;
            const projectName = projects.find((p) => p.id === b.projectId)?.name ?? null;
            const editorId = `ap-editor-${b.id}`;
            return (
              <li key={b.id} className={`ap-item${b.hidden ? ' is-hidden' : ''}${open ? ' is-open' : ''}${confirmId === b.id ? ' is-confirming' : ''}`} data-testid="action-edit" data-action={b.seedId ?? b.id}>
                <div className="ap-row">
                  <button
                    type="button"
                    className="ap-summary"
                    data-testid="action-summary"
                    aria-expanded={open}
                    aria-controls={editorId}
                    onClick={() => setOpenId(open ? null : b.id)}
                  >
                    {open ? <IconChevronDown /> : <IconChevronRight />}
                    <span className="ap-label">{b.label.trim() === '' ? 'Untitled button' : b.label}</span>
                    <span className="ap-tag" title="Where the button shows">
                      {b.hidden ? 'Hidden' : showsIn(b)}
                    </span>
                    <span className="ap-tag" title="Where the button sends its message">
                      {sendsTo(b, projectName)}
                    </span>
                    <span className="ap-preview">{previewLine(b.text) || 'No message yet'}</span>
                  </button>
                  {confirmId === b.id ? (
                    <div className="ap-confirm" role="group" aria-label={`Delete ${name}?`}>
                      <span>Delete {name}?</span>
                      <Button
                        variant="secondary"
                        danger
                        data-testid="action-delete-confirm"
                        onClick={() => {
                          setButtons((l) => removeButton(l, b.id));
                          setConfirmId(null);
                          if (open) setOpenId(null);
                          requestAnimationFrame(() => document.getElementById('ap-add')?.focus());
                        }}
                      >
                        Delete
                      </Button>
                      {/* The safe choice takes focus, so the keyboard lands inside the question. */}
                      <Button
                        variant="text"
                        autoFocus
                        onClick={() => {
                          setConfirmId(null);
                          requestAnimationFrame(() => document.getElementById(`ap-delete-${b.id}`)?.focus());
                        }}
                      >
                        Cancel
                      </Button>
                    </div>
                  ) : (
                    <div className="ap-tools">
                      <Button variant="icon" data-testid="action-up" disabled={i === 0} label={`Move ${name} up`} onClick={() => setButtons((l) => moveButton(l, b.id, -1))}>
                        <IconChevronUp size={15} />
                      </Button>
                      <Button variant="icon" data-testid="action-down" disabled={i === buttons.length - 1} label={`Move ${name} down`} onClick={() => setButtons((l) => moveButton(l, b.id, 1))}>
                        <IconChevronDown size={15} />
                      </Button>
                      <Button
                        variant="icon"
                        data-testid="action-hide"
                        label={b.hidden ? `Show ${name} again` : `Hide ${name}`}
                        onClick={() => setButtons((l) => updateButton(l, b.id, { hidden: !b.hidden }))}
                      >
                        {b.hidden ? <IconEyeOff size={15} /> : <IconEye size={15} />}
                      </Button>
                      <Button variant="icon" danger id={`ap-delete-${b.id}`} data-testid="action-delete" label={`Delete ${name}`} onClick={() => setConfirmId(b.id)}>
                        <IconTrash size={15} />
                      </Button>
                    </div>
                  )}
                </div>
                {bad && (
                  <div className="error ap-problem" role="alert">
                    {bad}
                  </div>
                )}
                {open && (
                  <div className="ap-editor" id={editorId}>
                    <div className="ap-field">
                      <label className="ap-field-label" htmlFor={`label-${b.id}`}>
                        Label
                      </label>
                      <TextInput
                        id={`label-${b.id}`}
                        type="text"
                        value={b.label}
                        maxLength={80}
                        onChange={(e) => setButtons((l) => updateButton(l, b.id, { label: e.target.value }))}
                      />
                    </div>
                    <div className="ap-fields">
                      <div className="ap-field">
                        <span className="ap-field-label" aria-hidden="true">
                          Shows in
                        </span>
                        <Segmented
                          label="Shows in"
                          value={b.placement}
                          options={[
                            { value: 'topbar', label: 'Top bar' },
                            { value: 'overflow', label: 'More menu' },
                          ]}
                          onChange={(placement) => setButtons((l) => updateButton(l, b.id, { placement }))}
                        />
                      </div>
                      <div className="ap-field">
                        <span className="ap-field-label" aria-hidden="true">
                          Sends to
                        </span>
                        <Segmented
                          label="Sends to"
                          value={b.kind}
                          options={[
                            { value: 'prompt', label: 'This chat' },
                            { value: 'new_chat', label: 'A new chat' },
                          ]}
                          onChange={(kind) => setButtons((l) => updateButton(l, b.id, { kind }))}
                        />
                      </div>
                      {b.kind === 'new_chat' && (
                        <div className="ap-field">
                          <label className="ap-field-label" htmlFor={`project-${b.id}`}>
                            Project
                          </label>
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
                    </div>
                    <div className="ap-field">
                      <label className="ap-field-label" htmlFor={`text-${b.id}`}>
                        Message
                      </label>
                      <TextArea id={`text-${b.id}`} value={b.text} onChange={(e) => setButtons((l) => updateButton(l, b.id, { text: e.target.value }))} />
                    </div>
                  </div>
                )}
              </li>
            );
          })}
        </ul>
      )}
      <div className="ap-footer">
        <Button variant="text" id="ap-add" data-testid="action-add" disabled={buttons.length >= MAX_BUTTONS} onClick={add}>
          Add a button
        </Button>
        <Button variant="primary" data-testid="actions-save" disabled={!dirty || problem !== null} onClick={() => void save()}>
          Save
        </Button>
        <Button
          variant="text"
          disabled={!dirty}
          onClick={() => {
            setButtons(saved);
            setConfirmId(null);
          }}
        >
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
