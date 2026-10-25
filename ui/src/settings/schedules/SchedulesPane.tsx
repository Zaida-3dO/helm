// Settings → Schedules: the ready-made schedules (off until turned on for a chat), then your own.
// Every change goes to the host; the list follows its `schedules` event.

import { useEffect, useState, useSyncExternalStore } from 'react';
import type { Run, RunStatus, Schedule, Template } from '../../schedules/api.ts';
import { destinationText, statusText, timingText } from '../../schedules/model.ts';
import { scheduleStoreFor, type ScheduleStore } from '../../schedules/store.ts';
import { useSelection, useShell } from '../../shell/context.ts';
import { Button, Checkbox, Picker, toast } from '../../ui/index.ts';
import { IconCheck, IconClock, IconRemind } from '../../ui/icons.tsx';
import { projectOps } from '../../api/projects.ts';
import { ScheduleEditor, type EditorStart, type ProjectChoice, type SavePatch } from './ScheduleEditor.tsx';
import '../../schedules/schedules.css';
import './schedules-pane.css';

const RUN_LABEL: Record<RunStatus, string> = {
  dispatching: 'Sending',
  started: 'Sent',
  failed: 'Not sent',
  blocked: 'Chat unavailable',
  uncertain: 'Unknown (Helm closed while sending)',
};

/** The machine's time zone, e.g. "Europe/London (UTC+01:00)". */
function timeZoneText(): string {
  const name = Intl.DateTimeFormat().resolvedOptions().timeZone;
  const off = -new Date().getTimezoneOffset();
  const sign = off < 0 ? '-' : '+';
  const hh = String(Math.floor(Math.abs(off) / 60)).padStart(2, '0');
  const mm = String(Math.abs(off) % 60).padStart(2, '0');
  return `${name}, UTC${sign}${hh}:${mm}`;
}

function errorText(e: unknown): string {
  return String((e as Error)?.message ?? e);
}

function requestId(): string {
  return globalThis.crypto.randomUUID();
}

export function SchedulesPane() {
  const shell = useShell();
  const selected = useSelection().chatId;
  const store = scheduleStoreFor(shell.api, shell.bus);
  useEffect(() => {
    store.start();
  }, [store]);
  const list = useSyncExternalStore(store.subscribe, () => store.list);
  const error = useSyncExternalStore(store.subscribe, () => store.error);
  const chats = useSyncExternalStore(shell.store.subscribe, () => shell.store.chatList());
  const [now, setNow] = useState(() => Date.now());
  useEffect(() => {
    const t = setInterval(() => setNow(Date.now()), 15_000);
    return () => clearInterval(t);
  }, []);
  useEffect(() => setNow(Date.now()), [list]);
  const [editing, setEditing] = useState<EditorStart | null>(null);
  const reminders = useSyncExternalStore(store.subscribe, () => store.reminders);
  // Folder projects a new-chat schedule can start chats in, read when the editor opens.
  const [projects, setProjects] = useState<ProjectChoice[]>([]);
  const editorOpen = editing !== null;
  useEffect(() => {
    if (!editorOpen) return;
    projectOps(shell.api)
      .list()
      .then((all) => setProjects(all.filter((p) => p.kind === 'folder' && p.path && !p.archived).map((p) => ({ id: p.id, name: p.name }))))
      .catch(() => setProjects([]));
  }, [editorOpen, shell.api]);
  const [showInactive, setShowInactive] = useState(false);

  const all = list?.schedules ?? [];
  const live = all.filter((s) => s.state === 'active' || s.state === 'paused');
  const shown = showInactive ? all : live;
  const defaultChat = selected && chats.some((c) => c.id === selected) ? selected : (chats[0]?.id ?? null);

  const save = async (patch: SavePatch) => {
    const target = editing;
    if (!target) return;
    if (target.schedule) {
      const s = target.schedule;
      await store.update({
        scheduleId: s.id,
        revision: s.revision,
        title: patch.title,
        message: patch.message,
        kind: patch.kind,
        destination: patch.destination,
        chatId: patch.chatId,
        projectId: patch.projectId,
        ...(patch.timingChanged ? { timing: patch.timing } : {}),
      });
    } else {
      await store.create({
        requestId: requestId(),
        title: patch.title,
        message: patch.message,
        timing: patch.timing,
        kind: patch.kind,
        destination: patch.destination,
        chatId: patch.chatId,
        ...(patch.projectId ? { projectId: patch.projectId } : {}),
        ...(target.template ? { template: target.template } : {}),
      });
    }
    setEditing(null);
  };

  return (
    <div className="sched-pane" data-testid="schedules-pane">
      <p className="sched-intro">
        A schedule sends a saved message into a chat on a timer, starts a new chat with it each time, or just reminds you. It only runs while Helm is open and your computer is awake.
        If runs were missed, you get one, not one per missed time, and a chat that is busy gets its message when it is free.
      </p>
      <p className="sched-fine" data-testid="schedule-zone">
        Times are in your computer's time zone ({timeZoneText()}).
      </p>
      {reminders.length > 0 && (
        <section data-testid="reminder-list">
          <div className="sched-head">
            <h3>Reminders</h3>
            <Button variant="text" onClick={() => void store.dismiss().catch((e) => toast.show({ kind: 'error', text: errorText(e) }))} data-testid="reminder-dismiss-all">
              Dismiss all
            </Button>
          </div>
          <ul className="sched-rows">
            {reminders.map((r) => (
              <li key={r.id} className="sched-row" data-testid="reminder-row" data-reminder-id={r.id}>
                <div className="sched-row-main">
                  <div className="sched-row-title">{r.title}</div>
                  <div className="sched-reminder-note">{r.message}</div>
                  <div className="sched-fine">{new Date(r.createdAt).toLocaleString()}</div>
                </div>
                <div className="sched-row-actions">
                  {r.chatId && (
                    <Button variant="text" onClick={() => shell.select(r.chatId)} data-testid={`reminder-${r.id}-open`}>
                      Open {r.chatTitle || 'chat'}
                    </Button>
                  )}
                  <Button variant="text" onClick={() => void store.dismiss(r.id).catch((e) => toast.show({ kind: 'error', text: errorText(e) }))} data-testid={`reminder-${r.id}-dismiss`}>
                    <IconCheck size={13} />
                    Dismiss
                  </Button>
                </div>
              </li>
            ))}
          </ul>
        </section>
      )}
      {error && (
        <p className="sched-notice" data-testid="schedules-error">
          Schedules could not be read: {error}
        </p>
      )}

      <section data-testid="schedule-templates">
        <h3>Ready-made</h3>
        <p className="sched-fine">Off until you turn one on for a chat.</p>
        <div className="sched-templates">
          {(list?.templates ?? []).map((t) => (
            <TemplateCard
              key={t.id}
              template={t}
              chats={chats}
              defaultChat={defaultChat}
              store={store}
              enabledFor={live.filter((s) => s.definition.template === t.id)}
              onCustomise={(chatId) => setEditing({ title: t.title, message: t.message, chatId, timing: t.timing, template: t.id })}
            />
          ))}
        </div>
      </section>

      <section data-testid="schedule-list">
        <div className="sched-head">
          <h3>Your schedules</h3>
          <Button variant="secondary" onClick={() => setEditing({ title: '', message: '', chatId: defaultChat, timing: null })} data-testid="schedule-new">
            New schedule
          </Button>
        </div>
        {list && shown.length === 0 && (
          <div className="sched-empty" data-testid="schedule-empty">
            <IconClock size={20} />
            <strong>No schedules</strong>
            <span>Turn on a ready-made one above, or make your own.</span>
          </div>
        )}
        <ul className="sched-rows">
          {shown.map((s) => (
            <ScheduleRow key={s.id} schedule={s} store={store} now={now} onEdit={() => setEditing({ schedule: s, title: s.definition.title, message: s.definition.message, chatId: s.definition.chatId, timing: s.definition.timing, kind: s.definition.kind, destination: s.definition.destination, projectId: s.definition.projectId ?? null })} />
          ))}
        </ul>
        {all.length > live.length && (
          <Checkbox label="Show finished and cancelled" checked={showInactive} onChange={setShowInactive} data-testid="schedule-show-inactive" />
        )}
      </section>

      {editing && <ScheduleEditor start={editing} chats={chats} projects={projects} now={now} onSave={save} onClose={() => setEditing(null)} />}
    </div>
  );
}

function TemplateCard({
  template,
  chats,
  defaultChat,
  store,
  enabledFor,
  onCustomise,
}: {
  template: Template;
  chats: { id: string; title: string }[];
  defaultChat: string | null;
  store: ScheduleStore;
  enabledFor: Schedule[];
  onCustomise: (chatId: string | null) => void;
}) {
  const [picked, setPicked] = useState<string | null>(null);
  const [busy, setBusy] = useState(false);
  const chatId = picked ?? defaultChat;
  const turnOn = async () => {
    if (!chatId) return;
    setBusy(true);
    try {
      await store.create({ requestId: requestId(), title: template.title, message: template.message, timing: template.timing, chatId, template: template.id });
    } catch (e) {
      toast.show({ kind: 'error', text: errorText(e) });
    } finally {
      setBusy(false);
    }
  };
  return (
    <div className="sched-template" data-testid={`template-${template.id}`}>
      <div className="sched-template-title">{template.title}</div>
      <div className="sched-fine">{template.summary}</div>
      <div className="sched-template-actions">
        <Picker<string>
          label={`Chat for ${template.title}`}
          value={chatId}
          options={chats.map((c) => ({ value: c.id, title: c.title || 'Untitled chat', text: c.title }))}
          onChange={setPicked}
          searchable
          placeholder={chats.length === 0 ? 'No chats yet' : 'Pick a chat'}
          testId={`template-${template.id}-chat`}
        />
        <Button variant="primary" disabled={!chatId || busy} onClick={() => void turnOn()} data-testid={`template-${template.id}-on`}>
          Turn on
        </Button>
        <Button variant="text" disabled={!chatId} onClick={() => onCustomise(chatId)} data-testid={`template-${template.id}-customise`}>
          Customise…
        </Button>
      </div>
      {enabledFor.length > 0 && (
        <div className="sched-fine" data-testid={`template-${template.id}-enabled`}>
          On for: {enabledFor.map((s) => s.chatTitle || 'a chat').join(', ')}
        </div>
      )}
    </div>
  );
}

function ScheduleRow({ schedule: s, store, now, onEdit }: { schedule: Schedule; store: ScheduleStore; now: number; onEdit: () => void }) {
  const [runs, setRuns] = useState<Run[] | null>(null);
  const [open, setOpen] = useState(false);
  const [confirm, setConfirm] = useState(false);
  const [busy, setBusy] = useState(false);
  const inactive = s.state === 'completed' || s.state === 'cancelled';

  useEffect(() => {
    if (!open) return;
    let live = true;
    store.runs(s.id).then(
      (r) => live && setRuns(r.runs),
      (e) => live && toast.show({ kind: 'error', text: errorText(e) }),
    );
    // Re-read when the schedule moves (a run was claimed).
    return () => {
      live = false;
    };
  }, [open, store, s.id, s.revision]);

  const act = async (f: () => Promise<unknown>) => {
    setBusy(true);
    try {
      await f();
    } catch (e) {
      toast.show({ kind: 'error', text: errorText(e) });
    } finally {
      setBusy(false);
      setConfirm(false);
    }
  };

  return (
    <li className="sched-row" data-testid="schedule-row" data-schedule-id={s.id} data-state={s.state}>
      <div className="sched-row-main">
        <div className="sched-row-title">
          {s.definition.kind === 'reminder' && <IconRemind size={13} />} {s.definition.title}
        </div>
        <div className="sched-fine">
          {timingText(s.definition.timing)} · {destinationText(s)}
        </div>
        <div className="sched-fine" data-testid={`schedule-${s.id}-status`}>
          {statusText(s, now)}
        </div>
      </div>
      <div className="sched-row-actions">
        {!inactive && (
          <>
            <Button
              variant="text"
              disabled={busy}
              onClick={() => void act(() => store.update({ scheduleId: s.id, revision: s.revision, paused: s.state === 'active' }))}
              data-testid={`schedule-${s.id}-toggle`}
            >
              {s.state === 'active' ? 'Pause' : 'Resume'}
            </Button>
            <Button variant="text" disabled={busy} onClick={onEdit} data-testid={`schedule-${s.id}-edit`}>
              Edit
            </Button>
            <Button
              variant="text"
              danger
              disabled={busy}
              onClick={() => (confirm ? void act(() => store.cancel(s)) : setConfirm(true))}
              data-testid={`schedule-${s.id}-cancel`}
            >
              {confirm ? 'Confirm cancel' : 'Cancel'}
            </Button>
          </>
        )}
        <Button variant="text" onClick={() => setOpen((o) => !o)} data-testid={`schedule-${s.id}-runs-toggle`}>
          {open ? 'Hide runs' : 'Runs'}
        </Button>
      </div>
      {open && (
        <ul className="sched-runs" data-testid={`schedule-${s.id}-runs`}>
          {runs === null && <li className="sched-fine">Loading…</li>}
          {runs?.length === 0 && <li className="sched-fine">It has not run yet.</li>}
          {runs?.map((r) => (
            <li key={r.scheduledAt} className="sched-run" data-status={r.status}>
              <span>{new Date(r.scheduledAt).toLocaleString()}</span>
              <span>{RUN_LABEL[r.status]}</span>
              {r.error && <span className="sched-fine">{r.error}</span>}
            </li>
          ))}
        </ul>
      )}
    </li>
  );
}
