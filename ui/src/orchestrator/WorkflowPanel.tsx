// The workflow panel in the composer dock: the active
// task's six stages, what it needs from you, its changes with their gates, and a link to the Agent
// Standup item when the chat is linked. Read-only: the Allow cards for workflow kinds (land, ready,
// drop, skip, allowMore, reopen) are the existing consent cards in the consent slot above.

import { useEffect, useState, useSyncExternalStore } from 'react';
import type { AccessoryContext } from '../composer/slots.ts';
import { openItem } from '../itemlinks/store.ts';
import { useShell } from '../shell/context.ts';
import { toast } from '../ui/index.ts';
import { changeLabel, changeName, needsYou, prNumber, stageChips } from './progress.ts';
import { runStoreFor } from './runstore.ts';
import { orchestratorStoreFor } from './store.ts';
import './workflow-panel.css';

export function WorkflowPanel({ ctx }: { ctx: AccessoryContext }) {
  const shell = useShell();
  const runs = runStoreFor(shell.api, shell.bus);
  const modes = orchestratorStoreFor(shell.api, shell.bus);
  const chatId = ctx.chat.id;
  const [open, setOpen] = useState(false);
  // Ask again when a turn starts or ends: that is when the agent has moved the task on.
  useEffect(() => runs.ensure(chatId), [runs, chatId, ctx.busy]);
  useSyncExternalStore(runs.subscribe, () => runs.version);
  useSyncExternalStore(modes.subscribe, () => modes.version);

  const entry = runs.entry(chatId);
  const settings = modes.settings;
  if (!entry || !settings?.enabled || ctx.chat.harness !== 'claude') return null;
  const { view, allowRequests, item } = entry;
  const chips = stageChips(view, settings);
  if (!chips || !view.task) {
    if (view.otherTasks.length === 0) return null;
    return (
      <div className="orch-panel" data-testid="workflow-panel" data-status="none">
        <div className="orch-panel-head">
          <span className="orch-label">WORKFLOW</span>
          <span className="orch-panel-title">
            {view.otherTasks.length === 1 ? `1 task is parked: ${view.otherTasks[0]?.title}` : `${view.otherTasks.length} tasks are parked`}
          </span>
        </div>
      </div>
    );
  }
  const task = view.task;
  const needs = needsYou(view, settings, allowRequests);
  const itemId = item?.itemId ?? task.itemId;
  const itemTitle = item?.title ?? (task.itemId ? task.itemId.slice(0, 8) : '');
  return (
    <div className="orch-panel" data-testid="workflow-panel" data-status={task.status} data-needs={needs.length}>
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
        {needs.length > 0 && (
          <span className="orch-needs" data-testid="workflow-needs">
            Needs you · {needs.map((n) => n.text).join(' · ')}
          </span>
        )}
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
            <div key={`${c.repo}/${c.branch}`} className="orch-change" data-testid="workflow-change" data-state={c.state} data-branch={c.branch}>
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
          {view.otherTasks.map((o) => (
            <div key={o.runId} className="orch-line orch-other">
              <span className="orch-line-name">Other task</span>
              <span>
                {o.title} · {o.status}
                {o.parkReason ? ` (${o.parkReason})` : ''}
              </span>
            </div>
          ))}
        </div>
      )}
    </div>
  );
}
