// The queue panel: a modal over the host's queues, opened from the sidebar's Queue tile.
// Two tabs: the chat queue — chats working first, then chats whose turn start waits — and
// the build queue: features to run one after another, each started by the host when the one
// before it settles. Every chat row opens its chat and carries a tag when the chat belongs
// to another profile, and pause and stop reach any chat this PC runs, whoever's it is (the
// host stamps the acting profile on the target's transcript). The rows come straight from
// the host; every action answers with the fresh queue.

import { useEffect, useMemo, useState, useSyncExternalStore } from 'react';
import type { BuildQueueEntry, ModelCatalogue, QueueActive, QueueEntry, QueueSummary } from '../api/generated/index.ts';
import type { ProjectNode } from '../api/projects.ts';
import type { ProfilesView } from '../api/profiles.ts';
import type { Api } from '../api/client.ts';
import type { EventBus } from '../api/events.ts';
import { chatOps } from '../api/ops.ts';
import type { QueueStore } from './store.ts';
import { BuildQueueStore } from './store.ts';
import { buildQueueOps } from './api.ts';
import { actionsFor, activeLine, aheadOf, buildActionsFor, buildLine, buildStateLabel, CLASS_LABEL, claudeLine, showsProfile, untilText, waitLine } from './model.ts';
import { Button, Dialog, dropBefore, toast, useDragReorder, type DragRowProps } from '../ui/index.ts';
import { IconChevronDown, IconChevronUp, IconExternalLink, IconPause, IconPlay, IconStop, IconX } from '../ui/icons.tsx';
import './queue.css';

type MoveAction = 'top' | 'up' | 'down' | 'move' | 'hold' | 'release' | 'cancel' | 'pause' | 'stop';

function toastError(e: unknown): void {
  toast.show({ kind: 'error', text: String((e as Error)?.message ?? e) });
}

/** How many rows stand on this entry's line, so Down can stop at the end. */
function lineSize(entries: readonly QueueEntry[], mine: QueueEntry): number {
  if (mine.reason === 'manual') return 1;
  return entries.filter((e) => e.reason !== 'manual' && e.serverId === mine.serverId).length;
}

/** A short readable name for a server id; unknown ids show as they are. */
function serverName(id: string): string {
  return id === 'strata' ? 'Strata' : id === 'ollama' ? 'Ollama' : id;
}

export function QueuePanel({
  store,
  api,
  bus,
  projects,
  currentProjectId,
  profiles,
  onOpen,
  onClose,
}: {
  store: QueueStore;
  api: Api;
  bus: EventBus;
  /** The projects a started build may run in (the build tab's picker). */
  projects: readonly ProjectNode[];
  /** The open chat's project: the picker starts on it. */
  currentProjectId: string | null;
  /** The profiles of this install: the tag on a chat that belongs to someone else. */
  profiles: ProfilesView | null;
  onOpen: (chatId: string) => void;
  onClose: () => void;
}) {
  const [tab, setTab] = useState<'chats' | 'build'>('chats');
  const summary = store.getSnapshot();
  const now = Date.now();
  const claudeUntil = summary != null && summary.claudeFull && summary.claudeUntil != null ? untilText(summary.claudeUntil, now) : null;
  // The build queue is kept fresh by the host's own event: while the panel stands, any
  // change to the line (an action here, a chat status change there) arrives as one ping.
  const buildStore = useMemo(() => new BuildQueueStore(buildQueueOps(api)), [api]);
  const buildEntries = useSyncExternalStore(buildStore.subscribe, buildStore.getSnapshot);
  useEffect(() => bus.on('build_queue_changed', () => buildStore.refresh()), [bus, buildStore]);
  const act = (chatId: string, action: MoveAction, beforeChatId?: string | null) => {
    store.act(chatId, action, beforeChatId).catch(toastError);
  };
  // Drag one row to stand where it was dropped: the host renumbers the line and answers
  // with the fresh queue, so the drop is only an intention until it agrees.
  const reorder = useDragReorder((from, to) => {
    if (!summary) return;
    const before = dropBefore(summary.entries, from, to);
    if (before === undefined) return;
    act(summary.entries[from]!.chatId, 'move', before?.chatId ?? null);
  });
  return (
    <Dialog
      title="Queue"
      onClose={onClose}
      wide
      padded
      testId="queue-panel"
      intro={
        tab === 'chats'
          ? 'Chats working now and chats whose turn is waiting to open. A waiting chat keeps its message saved and opens on its own when its place frees; pause and stop reach any chat this PC runs, whoever it belongs to.'
          : 'Features queued to build one after another. Helm starts a waiting entry only when the build before it has settled, one at a time.'
      }
    >
      <div className="queue-tabs" data-testid="queue-tabs">
        <button type="button" className={`queue-tab${tab === 'chats' ? ' on' : ''}`} onClick={() => setTab('chats')} data-testid="queue-tab-chats">Chats waiting</button>
        <button type="button" className={`queue-tab${tab === 'build' ? ' on' : ''}`} onClick={() => setTab('build')} data-testid="queue-tab-build">Build queue</button>
      </div>
      {tab === 'chats' && (
        <>
          {summary === null ? (
            <p className="queue-empty">Checking the queue…</p>
          ) : (
            <>
              <div className="queue-group" data-testid="queue-group-active">Working{summary.active.length > 0 ? ` · ${summary.active.length}` : ''}</div>
              {summary.active.length === 0 ? (
                <p className="queue-empty" data-testid="queue-active-empty">Nothing is working.</p>
              ) : (
                summary.active.map((a) => <ActiveRow key={a.chatId} entry={a} act={act} onOpen={onOpen} onClose={onClose} profiles={profiles} />)
              )}
              <div className="queue-group" data-testid="queue-group-queued">Waiting{summary.entries.length > 0 ? ` · ${summary.entries.length}` : ''}</div>
              {summary.entries.length === 0 ? (
                <p className="queue-empty" data-testid="queue-empty">Nothing is waiting.</p>
              ) : (
                summary.entries.map((e, i) => <QueueRow key={e.chatId} entry={e} index={i} summary={summary} claudeUntil={claudeUntil} act={act} onOpen={onOpen} onClose={onClose} reorder={reorder} profiles={profiles} />)
              )}
              {summary.claudeQueued > 0 || summary.claudeFull ? (
                <span className="queue-foot" data-testid="queue-claude-foot">{claudeLine(summary, now)}</span>
              ) : null}
            </>
          )}
        </>
      )}
      {tab === 'build' && <BuildQueueTab store={buildStore} entries={buildEntries} api={api} projects={projects} currentProjectId={currentProjectId} onOpen={onOpen} onClose={onClose} />}
    </Dialog>
  );
}

interface QueueReorder {
  rowProps: (index: number, draggable?: boolean) => DragRowProps;
  drag: { from: number; to: number; offset: number } | null;
}

/** The tag for a chat that belongs to another profile: the profile's own name. */
function ProfileChip({ profileId, profiles }: { profileId: string | null | undefined; profiles: ProfilesView | null }) {
  const currentId = profiles ? profiles.activeId ?? profiles.homeId : null;
  if (!showsProfile(profileId ?? null, currentId)) return null;
  const name = profiles?.profiles.find((p) => p.id === profileId)?.name;
  if (!name) return null;
  return (
    <span className="queue-tag" title={`This chat belongs to ${name}`} data-testid="queue-profile">
      {name}
    </span>
  );
}

/** A chat with a turn open: open it, pause it at its next boundary, or stop its worker. */
function ActiveRow({
  entry: a,
  act,
  onOpen,
  onClose,
  profiles,
}: {
  entry: QueueActive;
  act: (chatId: string, action: MoveAction, beforeChatId?: string | null) => void;
  onOpen: (chatId: string) => void;
  onClose: () => void;
  profiles: ProfilesView | null;
}) {
  return (
    <div className="queue-row chat-row" data-testid="queue-active-row" data-chat-id={a.chatId}>
      <button type="button" className="queue-open" title="Open this chat" onClick={() => { onClose(); onOpen(a.chatId); }}>
        <span className="queue-title">{a.title}</span>
        <IconExternalLink size={13} />
      </button>
      <span className="queue-line">{activeLine(a.serverId ? serverName(a.serverId) : null)}</span>
      <span className="queue-chips">
        <ProfileChip profileId={a.profileId} profiles={profiles} />
      </span>
      <div className="queue-actions">
        <Button variant="ghost" className="queue-btn" title="Pause this chat: it finishes the current step, stops at the next boundary and waits" onClick={() => act(a.chatId, 'pause')} data-testid="queue-pause">
          <IconPause size={13} />
        </Button>
        <Button variant="ghost" className="queue-btn" title="Stop this chat at once: its worker is taken down" onClick={() => act(a.chatId, 'stop')} data-testid="queue-stop">
          <IconStop size={13} />
        </Button>
      </div>
    </div>
  );
}

function QueueRow({
  entry: e,
  index,
  summary,
  claudeUntil,
  act,
  onOpen,
  onClose,
  reorder,
  profiles,
}: {
  entry: QueueEntry;
  index: number;
  summary: QueueSummary;
  claudeUntil: string | null;
  act: (chatId: string, action: MoveAction, beforeChatId?: string | null) => void;
  onOpen: (chatId: string) => void;
  onClose: () => void;
  reorder: QueueReorder;
  profiles: ProfilesView | null;
}) {
  const ahead = aheadOf(summary.entries, e.chatId);
  const last = ahead >= lineSize(summary.entries, e) - 1;
  const live = reorder.drag?.from === index;
  return (
    <div
      className={`queue-row chat-row${live ? ' drag-live' : ''}`}
      style={live ? { transform: `translateY(${reorder.drag!.offset}px)` } : undefined}
      {...reorder.rowProps(index, actionsFor(e).drag)}
      data-testid="queue-row"
      data-chat-id={e.chatId}
      data-reason={e.reason}
      data-dragging={live ? 'true' : undefined}
    >
      <button type="button" className="queue-open" title="Open this chat" onClick={() => { onClose(); onOpen(e.chatId); }}>
        <span className="queue-title">{e.title}</span>
        <IconExternalLink size={13} />
      </button>
      <span className="queue-line">{waitLine(e, ahead, e.reason === 'claude_capacity' ? claudeUntil : null, e.serverId ? serverName(e.serverId) : null)}</span>
      <span className="queue-chips">
        <ProfileChip profileId={e.profileId} profiles={profiles} />
        <span className="queue-class" title={`This start is from ${CLASS_LABEL[e.class] ?? e.class}`}>{e.class}</span>
      </span>
      <div className="queue-actions">
        <Button variant="ghost" className="queue-btn" title="Push to the front of its line" onClick={() => act(e.chatId, 'top')} data-testid="queue-top">
          To top
        </Button>
        <Button
          variant="ghost"
          className="queue-btn"
          title="Move one place up"
          onClick={() => act(e.chatId, 'up')}
          disabled={e.reason === 'manual' || ahead === 0}
          data-testid="queue-up"
        >
          <IconChevronUp size={13} />
        </Button>
        <Button
          variant="ghost"
          className="queue-btn"
          title="Move one place down"
          onClick={() => act(e.chatId, 'down')}
          disabled={e.reason === 'manual' || last}
          data-testid="queue-down"
        >
          <IconChevronDown size={13} />
        </Button>
        {e.reason === 'manual' ? (
          <Button variant="ghost" className="queue-btn" title="End the hand-parked wait" onClick={() => act(e.chatId, 'release')} data-testid="queue-release">
            <IconPlay size={13} />
          </Button>
        ) : (
          <Button variant="ghost" className="queue-btn" title="Pause this chat until you release it" onClick={() => act(e.chatId, 'hold')} data-testid="queue-hold">
            <IconPause size={13} />
          </Button>
        )}
        <Button variant="ghost" className="queue-btn" title="Stop this chat: its waiting send goes back to its owner" onClick={() => act(e.chatId, 'stop')} data-testid="queue-stop">
          <IconStop size={13} />
        </Button>
      </div>
    </div>
  );
}

/** The Build queue tab: the line of features to run one after another, and the form that
 *  joins one at the back. The host starts a waiting entry's chat itself, one at a time,
 *  and pings the panel when the line moves. */
/** The projects a started chat can run in: a folder project with a folder, or a board that has one. */
function buildProjects(projects: readonly ProjectNode[]): ProjectNode[] {
  return projects.filter((p) => !p.archived && !p.closed && (p.kind === 'folder' ? !!p.path : p.primaryFolderProjectId != null));
}

function BuildQueueTab({
  store,
  entries,
  api,
  projects,
  currentProjectId,
  onOpen,
  onClose,
}: {
  store: BuildQueueStore;
  entries: BuildQueueEntry[] | null;
  api: Api;
  projects: readonly ProjectNode[];
  currentProjectId: string | null;
  onOpen: (chatId: string) => void;
  onClose: () => void;
}) {
  const [title, setTitle] = useState('');
  // An entry names the project its chat runs in; the host refuses to start one that names none,
  // so the picker starts on the open chat's project (else the first that can take a build).
  const pickable = useMemo(() => buildProjects(projects), [projects]);
  const [pickedProject, setPickedProject] = useState('');
  const projectId = pickable.some((p) => p.id === pickedProject) ? pickedProject : pickable.some((p) => p.id === currentProjectId) ? currentProjectId! : (pickable[0]?.id ?? '');
  const [brief, setBrief] = useState('');
  const [itemId, setItemId] = useState('');
  const [model, setModel] = useState('');
  const [catalogue, setCatalogue] = useState<ModelCatalogue | null>(null);
  const ops = useMemo(() => chatOps(api), [api]);

  // The model names come from the same catalogue the composer's picker reads.
  useEffect(() => {
    let live = true;
    ops
      .catalogue()
      .then((c) => live && setCatalogue(c))
      .catch(() => {
        /* without the list the form still offers the default model */
      });
    return () => {
      live = false;
    };
  }, [ops]);

  const act = (run: Promise<unknown>) => {
    run.catch(toastError);
  };
  // Drag the same way as the chat queue: the hook measures every row, only entries still
  // in line can be picked up, and the drop is an intention until the host agrees.
  const reorder = useDragReorder((from, to) => {
    if (!entries) return;
    const before = dropBefore(entries, from, to);
    if (before === undefined) return;
    act(store.move(entries[from]!.id, before?.id ?? null));
  });

  const trimmedTitle = title.trim();
  const trimmedBrief = brief.trim();
  const trimmedItem = itemId.trim();
  const canAdd = trimmedTitle !== '' && (trimmedBrief !== '' || trimmedItem !== '') && projectId !== '';
  const submit = () => {
    if (!canAdd) return;
    act(
      store.add({
        title: trimmedTitle,
        ...(trimmedBrief !== '' ? { brief: trimmedBrief } : {}),
        ...(trimmedItem !== '' ? { itemId: trimmedItem } : {}),
        projectId,
        ...(model !== '' ? { model } : {}),
      }),
    );
    setTitle('');
    setBrief('');
    setItemId('');
  };

  return (
    <>
      <div className="build-queue-form" data-testid="build-queue-form">
        <div className="build-queue-form-row">
          <input className="ui-input" placeholder="Title" value={title} onChange={(e) => setTitle(e.target.value)} data-testid="build-queue-add-title" />
          <select className="ui-input build-queue-model" value={projectId} onChange={(e) => setPickedProject(e.target.value)} aria-label="Project to run in" title="The project the started chat runs in" data-testid="build-queue-add-project">
            {pickable.length === 0 && <option value="">No project to run in</option>}
            {pickable.map((p) => (
              <option key={p.id} value={p.id}>{p.name}</option>
            ))}
          </select>
          <select className="ui-input build-queue-model" value={model} onChange={(e) => setModel(e.target.value)} data-testid="build-queue-add-model">
            <option value="">Default model</option>
            {(catalogue?.models ?? []).map((m) => (
              <option key={m.id} value={m.id}>{m.name}</option>
            ))}
            {(catalogue?.local ?? []).map((m) => (
              <option key={m.id} value={m.id}>{`${m.server}: ${m.name}`}</option>
            ))}
          </select>
          <Button variant="primary" className="queue-btn" title="Join the back of the line" onClick={submit} disabled={!canAdd} data-testid="build-queue-add">
            Add
          </Button>
        </div>
        <textarea className="ui-textarea" placeholder="Brief — what the started chat opens with" rows={2} value={brief} onChange={(e) => setBrief(e.target.value)} data-testid="build-queue-add-brief" />
        <div className="build-queue-form-row">
          <input className="ui-input" placeholder="Board item id (used when there is no brief)" value={itemId} onChange={(e) => setItemId(e.target.value)} data-testid="build-queue-add-item" />
        </div>
      </div>
      {entries === null ? (
        <p className="queue-empty">Checking the build queue…</p>
      ) : entries.length === 0 ? (
        <p className="queue-empty" data-testid="build-queue-empty">No features are queued to build.</p>
      ) : (
        entries.map((e, i) => (
          <BuildQueueRow key={e.id} entry={e} index={i} entries={entries} act={act} onOpen={onOpen} onClose={onClose} reorder={reorder} store={store} />
        ))
      )}
      <span className="queue-foot">The host runs one feature at a time and starts the next when a build has been quiet for a while.</span>
    </>
  );
}

function BuildQueueRow({
  entry: e,
  index,
  entries,
  act,
  onOpen,
  onClose,
  reorder,
  store,
}: {
  entry: BuildQueueEntry;
  index: number;
  entries: readonly BuildQueueEntry[];
  act: (run: Promise<unknown>) => void;
  onOpen: (chatId: string) => void;
  onClose: () => void;
  reorder: ReturnType<typeof useDragReorder>;
  store: BuildQueueStore;
}) {
  const actions = buildActionsFor(e);
  const live = reorder.drag?.from === index;
  return (
    <div
      className={`queue-row build-queue-row${live ? ' drag-live' : ''}`}
      style={live ? { transform: `translateY(${reorder.drag!.offset}px)` } : undefined}
      {...reorder.rowProps(index, actions.reorder)}
      data-testid="build-queue-row"
      data-entry-id={e.id}
      data-state={e.state}
      data-dragging={live ? 'true' : undefined}
    >
      {actions.open && e.chatId ? (
        <button type="button" className="queue-open" title="Open the chat carrying this build" onClick={() => { onClose(); onOpen(e.chatId!); }} data-testid="build-queue-open">
          <span className="queue-title">{e.title}</span>
          <IconExternalLink size={13} />
        </button>
      ) : (
        <span className="queue-open queue-title-plain">{e.title}</span>
      )}
      <span className="queue-line">{buildLine(e)}</span>
      <span className="queue-class" data-testid="build-queue-state">{buildStateLabel(e.state)}</span>
      <div className="queue-actions">
        <Button
          variant="ghost"
          className="queue-btn"
          title="Move one place up"
          onClick={() => act(store.move(e.id, entries[index - 1]?.id ?? null))}
          disabled={!actions.reorder || index === 0}
          data-testid="build-queue-up"
        >
          <IconChevronUp size={13} />
        </Button>
        <Button
          variant="ghost"
          className="queue-btn"
          title="Move one place down"
          onClick={() => act(store.move(e.id, entries[index + 2]?.id ?? null))}
          disabled={!actions.reorder || index === entries.length - 1}
          data-testid="build-queue-down"
        >
          <IconChevronDown size={13} />
        </Button>
        {actions.pause ? (
          <Button variant="ghost" className="queue-btn" title="Pause this entry: the queue leaves it alone where it stands" onClick={() => act(store.pause(e.id))} data-testid="build-queue-pause">
            <IconPause size={13} />
          </Button>
        ) : actions.resume ? (
          <Button variant="ghost" className="queue-btn" title="Put this entry back in line" onClick={() => act(store.resume(e.id))} data-testid="build-queue-resume">
            <IconPlay size={13} />
          </Button>
        ) : null}
        <Button variant="ghost" className="queue-btn" title="Take this entry out of the list" onClick={() => act(store.remove(e.id))} data-testid="build-queue-remove">
          <IconX size={13} />
        </Button>
      </div>
    </div>
  );
}
