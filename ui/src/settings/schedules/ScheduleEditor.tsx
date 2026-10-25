// The schedule editor dialog: name, chat, message and when. Used for a new schedule, for editing one
// and for tweaking a ready-made one before it is turned on. Only the kit's controls.

import { useMemo, useState } from 'react';
import type { ChatRecord } from '../../api/generated/ChatRecord.ts';
import type { Destination, Schedule, ScheduleKind, Timing } from '../../schedules/api.ts';
import {
  DAY_LABELS,
  defaultDraft,
  draftToTiming,
  newChatPaceProblem,
  sameTiming,
  textProblem,
  timingToDraft,
  type Draft,
  type Kind,
} from '../../schedules/model.ts';
import { Button, Checkbox, Dialog, Picker, Segmented, SettingRow, TextArea, TextInput } from '../../ui/index.ts';
import '../../schedules/schedules.css';
import './schedules-pane.css';

/** A folder project a new-chat schedule can start its chats in. */
export interface ProjectChoice {
  id: string;
  name: string;
}

export interface EditorStart {
  /** Set when editing; absent for a new schedule or a template. */
  schedule?: Schedule;
  title: string;
  message: string;
  chatId: string | null;
  timing: Timing | null;
  kind?: ScheduleKind;
  destination?: Destination;
  projectId?: string | null;
  template?: string;
}

export interface SavePatch {
  title: string;
  message: string;
  kind: ScheduleKind;
  destination: Destination;
  /** The chat to send into or open; empty for a new-chat destination or a reminder with none. */
  chatId: string;
  /** The project a new-chat destination uses; empty otherwise. */
  projectId: string;
  timing: Timing;
  /** True when the timing differs from what the schedule had (an update then re-times it). */
  timingChanged: boolean;
}

export function ScheduleEditor({
  start,
  chats,
  projects,
  now,
  onSave,
  onClose,
}: {
  start: EditorStart;
  chats: ChatRecord[];
  projects: ProjectChoice[];
  now: number;
  onSave: (patch: SavePatch) => Promise<void>;
  onClose: () => void;
}) {
  const [title, setTitle] = useState(start.title);
  const [message, setMessage] = useState(start.message);
  const [chatId, setChatId] = useState<string | null>(start.chatId);
  const [kind, setKind] = useState<ScheduleKind>(start.kind ?? 'prompt');
  const [destination, setDestination] = useState<Destination>(start.destination ?? 'chat');
  const [projectId, setProjectId] = useState<string | null>(start.projectId ?? projects[0]?.id ?? null);
  const [draft, setDraft] = useState<Draft>(() => (start.timing ? timingToDraft(start.timing, now) : defaultDraft(now)));
  const [error, setError] = useState<string | null>(null);
  const [busy, setBusy] = useState(false);

  const reminder = kind === 'reminder';
  const newChat = !reminder && destination === 'newChat';
  const options = useMemo(
    () => chats.map((c) => ({ value: c.id, title: c.title || 'Untitled chat', text: c.title })),
    [chats],
  );
  // A destination that is archived or deleted is not in the list: keep it visible so it can be changed.
  const known = !chatId || options.some((o) => o.value === chatId);
  const withGone = known ? options : [{ value: chatId ?? '', title: start.schedule?.chatTitle || 'Unavailable chat', text: '' }, ...options];
  // A reminder's chat is optional, so it can be cleared.
  const shown = reminder ? [{ value: '', title: 'No chat', text: '' }, ...withGone] : withGone;

  const set = (patch: Partial<Draft>) => setDraft((d) => ({ ...d, ...patch }));

  const save = async () => {
    const problem = textProblem(title, message);
    if (problem) return setError(problem);
    if (kind === 'prompt' && destination === 'chat' && !chatId) return setError('Pick the chat to send into.');
    if (newChat && !projectId) return setError('Pick the project the new chats start in.');
    const t = draftToTiming(draft, now);
    if (!t.ok) return setError(t.error);
    const pace = newChat ? newChatPaceProblem(t.value) : null;
    if (pace) return setError(pace);
    setBusy(true);
    setError(null);
    try {
      await onSave({
        title: title.trim(),
        message,
        kind,
        destination: reminder ? 'chat' : destination,
        chatId: newChat ? '' : (chatId ?? ''),
        projectId: newChat ? (projectId ?? '') : '',
        timing: t.value,
        timingChanged: !start.schedule || !sameTiming(t.value, start.schedule.definition.timing),
      });
    } catch (e) {
      setError(String((e as Error)?.message ?? e));
      setBusy(false);
    }
  };

  return (
    <Dialog
      title={start.schedule ? 'Edit schedule' : 'New schedule'}
      onClose={onClose}
      busy={busy}
      testId="schedule-editor"
      intro={
        reminder
          ? 'Shows you a note at the chosen time. It only runs while Helm is open and your computer is awake; reminders missed meanwhile show once.'
          : 'Sends a saved message into a chat. It only runs while Helm is open and your computer is awake; runs missed meanwhile are sent once.'
      }
      footer={
        <>
          <Button variant="secondary" onClick={onClose} disabled={busy} data-testid="schedule-cancel-edit">
            Cancel
          </Button>
          <Button variant="primary" onClick={() => void save()} disabled={busy} data-testid="schedule-save">
            {start.schedule ? 'Save' : 'Create'}
          </Button>
        </>
      }
    >
      <div className="sched-form">
        <label className="sched-field">
          <span>Name</span>
          <TextInput value={title} onChange={(e) => setTitle(e.target.value)} maxLength={160} data-testid="schedule-title" />
        </label>
        <div className="sched-field">
          <span>What it does</span>
          <Segmented<ScheduleKind>
            label="What it does"
            testId="schedule-type"
            value={kind}
            onChange={setKind}
            options={[
              { value: 'prompt', label: 'Send a message' },
              { value: 'reminder', label: 'Remind me' },
            ]}
          />
        </div>
        {!reminder && (
          <div className="sched-field">
            <span>Send to</span>
            <Segmented<Destination>
              label="Where it goes"
              testId="schedule-destination"
              value={destination}
              onChange={setDestination}
              options={[
                { value: 'chat', label: 'An existing chat' },
                { value: 'newChat', label: 'A new chat each time' },
              ]}
            />
          </div>
        )}
        {newChat && (
          <p className="sched-fine" data-testid="schedule-newchat-note">
            Starts a fresh chat in the project on every run, so it can repeat at most once an hour.
          </p>
        )}
        {newChat ? (
          <div className="sched-field">
            <span>Project</span>
            <Picker<string>
              label="Project for the new chats"
              value={projectId}
              options={projects.map((p) => ({ value: p.id, title: p.name, text: p.name }))}
              onChange={setProjectId}
              searchable
              placeholder={projects.length === 0 ? 'No projects with a folder' : 'Pick a project'}
              testId="schedule-project"
            />
          </div>
        ) : (
          <div className="sched-field">
            <span>{reminder ? 'Chat to open (optional)' : 'Chat'}</span>
            <Picker<string>
              label={reminder ? 'Chat to open from the reminder' : 'Chat to send into'}
              value={chatId ?? (reminder ? '' : null)}
              options={shown}
              onChange={setChatId}
              searchable
              placeholder="Pick a chat"
              testId="schedule-chat"
            />
          </div>
        )}
        <label className="sched-field">
          <span>{reminder ? 'Note' : 'Message'}</span>
          <TextArea value={message} onChange={(e) => setMessage(e.target.value)} rows={5} data-testid="schedule-message" />
        </label>
        <div className="sched-field">
          <span>When</span>
          <Segmented<Kind>
            label="How often"
            testId="schedule-kind"
            value={draft.kind}
            onChange={(kind) => set({ kind })}
            options={[
              { value: 'interval', label: 'Every N minutes' },
              { value: 'weekly', label: 'Daily or weekly' },
              { value: 'once', label: 'Once' },
            ]}
          />
        </div>
        {draft.kind === 'interval' && (
          <SettingRow title="Minutes between runs" description="From 1 up to 525,600 (a year).">
            <TextInput
              type="number"
              min={1}
              step="any"
              value={draft.everyMinutes}
              onChange={(e) => set({ everyMinutes: e.target.value })}
              data-testid="schedule-every"
            />
          </SettingRow>
        )}
        {draft.kind === 'once' && (
          <SettingRow title="Date and time" description="Your local time.">
            <TextInput type="datetime-local" value={draft.onceAt} onChange={(e) => set({ onceAt: e.target.value })} data-testid="schedule-once" />
          </SettingRow>
        )}
        {draft.kind === 'weekly' && (
          <>
            <SettingRow title="Time of day" description="Your local time.">
              <TextInput type="time" value={draft.at} onChange={(e) => set({ at: e.target.value })} data-testid="schedule-time" />
            </SettingRow>
            <div className="sched-days" role="group" aria-label="Weekdays" data-testid="schedule-days">
              {DAY_LABELS.map((d, i) => (
                <Checkbox
                  key={d}
                  label={d}
                  checked={draft.days[i] ?? false}
                  onChange={(on) => set({ days: draft.days.map((v, j) => (j === i ? on : v)) })}
                  data-testid={`schedule-day-${i}`}
                />
              ))}
            </div>
          </>
        )}
        {error && (
          <p className="sched-error" role="alert" data-testid="schedule-error">
            {error}
          </p>
        )}
      </div>
    </Dialog>
  );
}
