// The workflow panel in the composer dock: the current
// task's six stages, what it needs from you, its changes with their gates, and a link to the Agent
// Standup item when the chat is linked. The buttons are the user's own steps:
// Sign off plan; Merge, Decline, Drop change, Retry land and Ready for review on a change; and the
// task's ⋯ menu (Skip a stage, Allow more rounds, Park, Reopen, Mark visual / Not visual, Drop
// change, End task). Each is a main-window op the host re-checks; a refusal is shown word for word.
// The Allow cards for workflow kinds (an agent asking) are the consent cards in the slot above.

import { useEffect, useRef, useState, useSyncExternalStore } from 'react';
import type { AccessoryContext } from '../composer/slots.ts';
import { openItem } from '../itemlinks/store.ts';
import { useShell } from '../shell/context.ts';
import { Button, ContextMenu, toast, type MenuItem } from '../ui/index.ts';
import { IconMore } from '../ui/icons.tsx';
import { workflowOps, type ChangeAction, type TaskAction, type TaskActionFields } from './api.ts';
import { canReopen, changeControls, changeLabel, changeName, needsYou, otherTasksWaiting, prNumber, signOffPlan, stageChips, taskMenu, type TaskMenuEntry } from './progress.ts';
import type { OtherTask, RunChange } from './run.ts';
import { runStoreFor } from './runstore.ts';
import { orchestratorStoreFor } from './store.ts';
import './workflow-panel.css';

export function WorkflowPanel({ ctx }: { ctx: AccessoryContext }) {
  const shell = useShell();
  const runs = runStoreFor(shell.api, shell.bus);
  const modes = orchestratorStoreFor(shell.api, shell.bus);
  const chatId = ctx.chat.id;
  const [open, setOpen] = useState(false);
  const [busy, setBusy] = useState<string | null>(null);
  const [menuAt, setMenuAt] = useState<{ x: number; y: number } | null>(null);
  const moreRef = useRef<HTMLButtonElement>(null);
  // Ask again when a turn starts or ends: that is when the agent has moved the task on.
  useEffect(() => runs.ensure(chatId), [runs, chatId, ctx.busy]);
  useSyncExternalStore(runs.subscribe, () => runs.version);
  useSyncExternalStore(modes.subscribe, () => modes.version);

  const entry = runs.entry(chatId);
  const settings = modes.settings;
  if (!entry || !settings?.enabled || ctx.chat.harness !== 'claude') return null;
  const { view, allowRequests, item } = entry;
  const ops = workflowOps(shell.api);
  /** Run one of the user's steps; the host's refusal is shown as it said it. */
  const act = async (key: string, call: () => Promise<unknown>) => {
    if (busy) return;
    setBusy(key);
    try {
      await call();
    } catch (e) {
      toast.show({ kind: 'error', text: (e as { message?: string })?.message ?? String(e) });
    } finally {
      setBusy(null);
      void runs.refresh(chatId);
    }
  };
  const onChange = (runId: string, c: RunChange, action: ChangeAction) =>
    void act(`${action}:${runId} ${c.target.repo} ${c.target.branch}`, () => ops.changeAction(runId, c.target.repo, c.target.branch, action));
  const onTask = (runId: string, action: TaskAction, fields?: TaskActionFields) => void act(`${action}:${runId}`, () => ops.taskAction(runId, action, fields));
  /** One change waiting on the user, with the host's buttons for it. */
  const waitingRow = (runId: string, c: RunChange) => (
    <div key={`${runId}/${c.target.repo}/${c.target.branch}`} className="orch-panel-action" data-testid="workflow-waiting" data-branch={c.branch}>
      <span className="orch-line-name">{changeName(c)}</span>
      {prNumber(c.pr) && <span className="orch-pr">{prNumber(c.pr)}</span>}
      <span>{changeLabel(c.state)}</span>
      {changeControls(c).map((b) => (
        <Button
          key={b.action}
          variant={b.primary ? 'primary' : 'secondary'}
          danger={b.danger}
          data-testid="workflow-change-action"
          data-action={b.action}
          disabled={busy !== null}
          title={`${b.label} ${changeName(c)}`}
          onClick={() => onChange(runId, c, b.action)}
        >
          {b.label}
        </Button>
      ))}
      {c.error && <span className="orch-error-line">{c.error}</span>}
    </div>
  );
  /** A task that is not the current one gets a row of its own: its title, and its changes that wait on the user. */
  const otherWaitingRows = otherTasksWaiting(view).map((o) => (
    <div key={o.runId} className="orch-other-task" data-testid="workflow-other-waiting" data-run-id={o.runId} data-status={o.status}>
      <div className="orch-panel-action">
        <span className="orch-line-name">{o.title}</span>
        <span>{o.status === 'handed_off' ? 'handed off' : o.status}</span>
        {o.parkReason && <span className="orch-gate">{o.parkReason}</span>}
      </div>
      {o.waiting.map((c) => waitingRow(o.runId, c))}
    </div>
  ));
  /** The line for any other task in the detail: its title and status, and Reopen when the user can bring it back. */
  const otherTaskLine = (o: OtherTask) => (
    <div key={o.runId} className="orch-line orch-other" data-testid="workflow-other-task" data-run-id={o.runId} data-status={o.status}>
      <span className="orch-line-name">Other task</span>
      <span>
        {o.title} · {o.status}
        {o.parkReason ? ` (${o.parkReason})` : ''}
      </span>
      {canReopen(o) && (
        <Button variant="secondary" data-testid="workflow-other-reopen" disabled={busy !== null} title={`Reopen ${o.title}`} onClick={() => onTask(o.runId, 'reopen')}>
          Reopen
        </Button>
      )}
    </div>
  );
  const chips = stageChips(view, settings);
  if (!chips || !view.task) {
    if (view.otherTasks.length === 0) return null;
    const needsOthers = needsYou(view, settings, allowRequests);
    return (
      <div className="orch-panel" data-testid="workflow-panel" data-status="none" data-needs={needsOthers.length} aria-busy={busy !== null}>
        <div className="orch-panel-head">
          <span className="orch-label">WORKFLOW</span>
          <span className="orch-panel-title">
            {view.otherTasks.length === 1 ? `1 task is waiting: ${view.otherTasks[0]?.title}` : `${view.otherTasks.length} tasks are waiting`}
          </span>
          {needsOthers.length > 0 && (
            <span className="orch-needs" data-testid="workflow-needs">
              Needs you · {needsOthers.map((n) => n.text).join(' · ')}
            </span>
          )}
        </div>
        {otherWaitingRows.length > 0 && (
          <div className="orch-panel-actions" data-testid="workflow-actions">
            {otherWaitingRows}
          </div>
        )}
        <div className="orch-panel-body" data-testid="workflow-body">
          {view.otherTasks.map(otherTaskLine)}
        </div>
      </div>
    );
  }
  const task = view.task;
  const signOff = signOffPlan(view, settings);
  const needs = needsYou(view, settings, allowRequests);
  const waiting = view.changes.filter((c) => changeControls(c).length > 0);
  const menu = taskMenu(view, settings);
  const toItem = (m: TaskMenuEntry): MenuItem => ({
    id: m.id,
    label: m.label,
    danger: m.danger,
    disabled: busy !== null,
    ...(m.submenu ? { submenu: m.submenu.map(toItem) } : {}),
    onSelect: () => {
      if (m.change) onChange(task.runId, m.change, 'drop');
      else if (m.action) onTask(task.runId, m.action, m.fields);
    },
  });
  const itemId = item?.itemId ?? task.itemId;
  const itemTitle = item?.title ?? (task.itemId ? task.itemId.slice(0, 8) : '');
  return (
    <div className="orch-panel" data-testid="workflow-panel" data-status={task.status} data-needs={needs.length} aria-busy={busy !== null}>
      <div className="orch-panel-head">
        <span className="orch-label">WORKFLOW</span>
        <button type="button" className="orch-panel-title" aria-expanded={open} data-testid="workflow-toggle" onClick={() => setOpen(!open)}>
          <span className="orch-caret" aria-hidden="true">
            {open ? '⌄' : '›'}
          </span>
          {task.title}
          {task.attempt > 1 ? ` · attempt ${task.attempt}` : ''}
          {task.visual ? ' · visual' : ''}
          {task.status !== 'active' ? ` · ${task.status}` : ''}
        </button>
        {itemId && (
          <button
            type="button"
            className="orch-item-link"
            data-testid="workflow-item"
            data-item-id={itemId}
            title="Open the Agent Standup item this chat is linked to"
            onClick={() => void openItem(shell.api, (id) => shell.select(id), itemId).then((err) => err && toast.show({ kind: 'error', text: err }))}
          >
            {itemTitle.length > 24 ? `${itemTitle.slice(0, 23)}…` : itemTitle}
          </button>
        )}
        {view.boardQueued > 0 && (
          <span className="orch-board-queue" data-testid="workflow-board-queue" title="Helm could not reach the task board. These records are kept and sent when it answers; it tries again every minute.">
            Board · {view.boardQueued} waiting
          </span>
        )}
        {needs.length > 0 && (
          <span className="orch-needs" data-testid="workflow-needs">
            Needs you · {needs.map((n) => n.text).join(' · ')}
          </span>
        )}
        {menu.length > 0 && (
          <Button
            ref={moreRef}
            variant="ghost"
            className="orch-more"
            data-testid="workflow-menu"
            aria-haspopup="menu"
            aria-expanded={menuAt !== null}
            label="Task actions"
            onClick={() => {
              const r = moreRef.current?.getBoundingClientRect();
              setMenuAt(menuAt ? null : { x: r ? r.right - 200 : 0, y: r ? r.bottom + 4 : 0 });
            }}
          >
            <IconMore />
          </Button>
        )}
        {menuAt && <ContextMenu x={menuAt.x} y={menuAt.y} label="Task actions" testId="workflow-task-menu" onClose={() => setMenuAt(null)} items={menu.map(toItem)} />}
      </div>
      <ol className="orch-stages-row" data-testid="workflow-stages" aria-label="Workflow stages">
        {chips.map((c) => (
          <li key={c.id} className="orch-chip" data-stage={c.id} data-state={c.state} data-testid="workflow-stage" aria-current={c.state === 'current' ? 'step' : undefined}>
            <span className="orch-chip-mark" aria-hidden="true">
              {c.state === 'done' ? '✓' : c.state === 'current' ? '●' : c.state === 'off' ? '–' : '○'}
            </span>
            {c.label}
            {c.detail && <span className="orch-chip-detail"> · {c.detail}</span>}
          </li>
        ))}
      </ol>
      {(signOff || waiting.length > 0 || otherWaitingRows.length > 0) && (
        <div className="orch-panel-actions" data-testid="workflow-actions">
          {signOff && (
            <div className="orch-panel-action">
              <span className="orch-line-name">Plan</span>
              <Button variant="primary" data-testid="workflow-signoff" disabled={busy !== null} onClick={() => onTask(task.runId, 'signOff', { planId: signOff })}>
                Sign off plan
              </Button>
            </div>
          )}
          {waiting.map((c) => waitingRow(task.runId, c))}
          {otherWaitingRows}
        </div>
      )}
      {open && (
        <div className="orch-panel-body" data-testid="workflow-body">
          {view.plan && (
            <div className="orch-line" data-testid="workflow-plan">
              <span className="orch-line-name">Plan</span>
              <span>
                {view.plan.id ? (view.plan.approved ? 'approved' : 'waiting for review') : 'not written yet'}
                {view.plan.id && view.plan.cap > 0 ? ` · ${view.plan.rounds} of ${view.plan.cap} review rounds` : ''}
                {view.plan.id && view.plan.approved && settings.planSignOff === 'me' ? (view.plan.signedOff ? ' · you signed off' : ' · waiting for your sign-off') : ''}
              </span>
              {view.plan.missing && <span className="orch-gate">{view.plan.missing}</span>}
            </div>
          )}
          {view.changes.map((c) => (
            <div key={`${c.target.repo}/${c.target.branch}`} className="orch-change" data-testid="workflow-change" data-state={c.state} data-branch={c.branch}>
              <div className="orch-line">
                <span className="orch-line-name">{changeName(c)}</span>
                <span data-testid="workflow-change-state">{changeLabel(c.state)}</span>
                {prNumber(c.pr) && <span className="orch-pr">{prNumber(c.pr)}</span>}
                {c.state === 'open' && c.tip && (
                  <span>
                    · {c.approved ? 'approved at this commit' : `review round ${c.reviewRounds} of ${c.reviewCap}`}
                  </span>
                )}
              </div>
              {c.error && <div className="orch-error-line">{c.error}</div>}
              {c.missing.map((m) => (
                <div key={m} className="orch-gate" data-testid="workflow-gate">
                  {m}
                </div>
              ))}
            </div>
          ))}
          {view.otherTasks.map(otherTaskLine)}
        </div>
      )}
    </div>
  );
}
