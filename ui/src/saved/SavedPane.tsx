// Settings → Saved: everything parked "for later". Search and paging, one-time reminders, a short
// note trail, Start a task (a new chat at the first project with the item in the draft), Done and
// Remove. Items arrive from the transcript's "Save for later" button and from a chat's save tool.

import { useCallback, useEffect, useMemo, useState } from 'react';
import { useShell } from '../shell/context.ts';
import { registerSettingsPane } from '../settings/registry.ts';
import { Button, PillTabs, TextInput, toast } from '../ui/index.ts';
import { dueLabel, draftFromItem, PAGE_SIZE, REMIND_PRESETS, remindAtFor, savedOps, STATE_TITLES, type SavedItem, type SavedNote, type SavedState } from './model.ts';
import './saved.css';

type Filter = 'all' | SavedState;

function errText(e: unknown): string {
  return String((e as Error).message ?? e);
}

function SavedPane() {
  const shell = useShell();
  const ops = useMemo(() => savedOps(shell.api), [shell]);
  const [items, setItems] = useState<SavedItem[]>([]);
  const [total, setTotal] = useState(0);
  const [page, setPage] = useState(1);
  const [query, setQuery] = useState('');
  const [filter, setFilter] = useState<Filter>('all');
  const [open, setOpen] = useState<string | null>(null);
  const [notes, setNotes] = useState<SavedNote[]>([]);
  const [draft, setDraft] = useState('');

  const load = useCallback(() => {
    void ops
      .list({
        ...(filter === 'all' ? {} : { state: filter }),
        ...(query.trim() ? { query: query.trim() } : {}),
        page,
      })
      .then((p) => {
        setItems(p.items);
        setTotal(p.total);
      })
      .catch(() => undefined);
  }, [ops, filter, query, page]);

  useEffect(() => {
    load();
    return shell.bus.on('saved', () => load());
  }, [load, shell]);

  const patch = (it: SavedItem, args: { state?: SavedState; remindAt?: number | null; remove?: boolean }) => {
    if (open === it.id) setOpen(null);
    ops
      .update({ id: it.id, ...args })
      .then(load)
      .catch((e: unknown) => toast.show({ kind: 'error', text: `Could not change the item: ${errText(e)}` }));
  };

  const start = (it: SavedItem) => {
    void (async () => {
      try {
        const projects = await shell.api<{ path: string | null }[]>('project_list', { includeArchived: false });
        const cwd = projects.find((p) => p.path)?.path;
        if (!cwd) {
          toast.show({ kind: 'error', text: 'Add a project folder first: a task needs somewhere to work.' });
          return;
        }
        const chat = await shell.api<{ id: string }>('chat_create', { cwd });
        await shell.api('draft_save', { chatId: chat.id, text: draftFromItem(it), revision: Date.now() });
        shell.select(chat.id);
        load();
      } catch (e) {
        toast.show({ kind: 'error', text: `Could not start the task: ${errText(e)}` });
      }
    })();
  };

  const openNotes = (id: string) => {
    setOpen(id);
    setNotes([]);
    setDraft('');
    void ops.comments({ id }).then((r) => setNotes(r.notes)).catch(() => undefined);
  };

  const addNote = (id: string) => {
    const text = draft.trim();
    if (!text) return;
    void ops
      .note({ id, text })
      .then(() => {
        setDraft('');
        return ops.comments({ id });
      })
      .then((r) => setNotes(r.notes))
      .catch((e: unknown) => toast.show({ kind: 'error', text: `Could not add the note: ${errText(e)}` }));
  };

  const pages = Math.max(1, Math.ceil(total / PAGE_SIZE));
  return (
    <div className="saved-pane" data-testid="saved-pane">
      <div className="saved-top">
        <TextInput
          placeholder="Search saved items"
          aria-label="Search saved items"
          value={query}
          onChange={(e) => {
            setQuery(e.target.value);
            setPage(1);
          }}
          data-testid="saved-search"
        />
        <PillTabs<Filter>
          label="Filter saved items"
          value={filter}
          onChange={(f) => {
            setFilter(f);
            setPage(1);
          }}
          testId="saved-filter"
          tabs={[
            { id: 'all', label: 'All' },
            { id: 'saved', label: 'Saved' },
            { id: 'filed', label: 'Filed' },
            { id: 'done', label: 'Done' },
            { id: 'dismissed', label: 'Dismissed' },
          ]}
        />
      </div>
      {items.length === 0 && (
        <p className="saved-empty">
          Nothing here yet. A chat reply's Save for later button, selected text in a chat, and a chat's save tool all
          put things here.
        </p>
      )}
      <ul className="saved-list">
        {items.map((it) => {
          const due = dueLabel(it, Date.now());
          return (
            <li key={it.id} className="saved-item" data-testid="saved-item" data-state={it.state}>
              <div className="saved-head">
                <span className="saved-title">{it.title}</span>
                {it.state !== 'saved' && <span className="saved-state">{STATE_TITLES[it.state]}</span>}
                {due && (
                  <span className="saved-due" data-testid="saved-due">
                    {due}
                  </span>
                )}
                {it.noteCount > 0 && (
                  <span className="saved-note-count">
                    {it.noteCount} note{it.noteCount === 1 ? '' : 's'}
                  </span>
                )}
              </div>
              <div className="saved-body">{it.body}</div>
              <div className="saved-actions">
                <Button variant="text" data-testid="saved-start" onClick={() => start(it)}>
                  Start task
                </Button>
                {REMIND_PRESETS.map((p) => (
                  <Button
                    key={p.id}
                    variant="text"
                    data-testid={`saved-remind-${p.id}`}
                    onClick={() => patch(it, { remindAt: remindAtFor(p.id, Date.now()) })}
                  >
                    {p.label}
                  </Button>
                ))}
                {due && (
                  <Button variant="text" data-testid="saved-remind-clear" onClick={() => patch(it, { remindAt: null })}>
                    Clear reminder
                  </Button>
                )}
                <Button variant="text" data-testid="saved-notes" onClick={() => (open === it.id ? setOpen(null) : openNotes(it.id))}>
                  Notes
                </Button>
                {it.state !== 'done' && (
                  <Button variant="text" data-testid="saved-done" onClick={() => patch(it, { state: 'done' })}>
                    Done
                  </Button>
                )}
                <Button variant="text" data-testid="saved-remove" onClick={() => patch(it, { remove: true })}>
                  Remove
                </Button>
              </div>
              {open === it.id && (
                <div className="saved-note-area" data-testid="saved-note-area">
                  {notes.length === 0 && <div className="saved-empty">No notes yet.</div>}
                  {notes.map((n) => (
                    <div key={n.id} className="saved-note">
                      <span className="saved-note-who">{n.authorType === 'user' ? 'You' : (n.authorId ?? 'A chat')}</span>
                      <span className="saved-note-text">{n.text}</span>
                    </div>
                  ))}
                  <div className="saved-note-add">
                    <TextInput
                      placeholder="Add a note"
                      aria-label="Add a note"
                      value={draft}
                      onChange={(e) => setDraft(e.target.value)}
                      onKeyDown={(e) => {
                        if (e.key === 'Enter') addNote(it.id);
                      }}
                      data-testid="saved-note-input"
                    />
                    <Button variant="primary" data-testid="saved-note-add" onClick={() => addNote(it.id)}>
                      Add
                    </Button>
                  </div>
                </div>
              )}
            </li>
          );
        })}
      </ul>
      {total > PAGE_SIZE && (
        <div className="saved-pager">
          <Button variant="text" disabled={page <= 1} onClick={() => setPage(page - 1)}>
            Newer
          </Button>
          <span>
            Page {page} of {pages}
          </span>
          <Button variant="text" disabled={page >= pages} onClick={() => setPage(page + 1)}>
            Older
          </Button>
        </div>
      )}
    </div>
  );
}

registerSettingsPane({ id: 'saved', title: 'Saved', order: 65, render: () => <SavedPane /> });
