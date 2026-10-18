// Settings → Orchestrator. One draft of the whole settings
// object; Save sends it whole (last writer wins, as the host does), Revert goes back to the last
// saved values. "Orchestrator for new chats" and Move fast live here too; Move fast is user-only.

import { useEffect, useState, useSyncExternalStore } from 'react';
import type { LandMode, MergeMethod, NewChats, StageUse, WorkflowSettings } from '../../orchestrator/api.ts';
import { assemble, textsOf, type Texts } from '../../orchestrator/draft.ts';
import { orchestratorStoreFor } from '../../orchestrator/store.ts';
import { useShell } from '../../shell/context.ts';
import { Button, Checkbox, Segmented, SettingRow, TextArea, TextInput } from '../../ui/index.ts';
import '../../orchestrator/orchestrator.css';
import './orchestrator-pane.css';

const USE_ALL: { value: StageUse; label: string }[] = [
  { value: 'required', label: 'Required' },
  { value: 'auto', label: 'Auto' },
  { value: 'off', label: 'Off' },
];
const USE_REVIEW = USE_ALL.filter((o) => o.value !== 'auto');

function errorText(e: unknown): string {
  return String((e as { message?: unknown })?.message ?? e);
}

function Rounds({ value, onChange, label }: { value: number; onChange: (n: number) => void; label: string }) {
  return (
    <label className="orch-rounds">
      Rounds
      <input
        type="number"
        min={1}
        max={20}
        aria-label={label}
        value={value}
        onChange={(e) => onChange(Math.max(1, Math.min(20, Math.round(Number(e.target.value)) || 1)))}
      />
    </label>
  );
}

interface StageLineProps {
  id: string;
  title: string;
  use: StageUse;
  options: { value: StageUse; label: string }[];
  agent: string;
  rounds?: number;
  onUse: (u: StageUse) => void;
  onAgent: (a: string) => void;
  onRounds?: (n: number) => void;
}

function StageLine(p: StageLineProps) {
  return (
    <div className="orch-stage" data-stage={p.id}>
      <div className="orch-stage-title">{p.title}</div>
      <Segmented<StageUse> label={`${p.title} use`} testId={`orch-use-${p.id}`} value={p.use} options={p.options} onChange={p.onUse} />
      <TextInput aria-label={`${p.title} agent`} value={p.agent} onChange={(e) => p.onAgent(e.target.value)} />
      {p.rounds !== undefined && p.onRounds && <Rounds label={`${p.title} rounds`} value={p.rounds} onChange={p.onRounds} />}
    </div>
  );
}

export function OrchestratorPane() {
  const shell = useShell();
  const store = orchestratorStoreFor(shell.api, shell.bus);
  const saved = useSyncExternalStore(store.subscribe, () => store.settings);
  const [draft, setDraft] = useState<WorkflowSettings | null>(null);
  const [texts, setTexts] = useState<Texts | null>(null);
  const [error, setError] = useState<string | null>(null);
  const [busy, setBusy] = useState(false);
  const [done, setDone] = useState(false);

  useEffect(() => {
    store.start();
    void store.loadSettings();
  }, [store]);
  // Take the host's values once, when the pane opens; later edits are the user's until Save or Revert.
  useEffect(() => {
    if (saved && !draft) {
      setDraft(saved);
      setTexts(textsOf(saved));
    }
  }, [saved, draft]);

  if (!draft || !texts) {
    return (
      <div className="orch-pane" data-testid="orchestrator-pane">
        <p className="orch-intro">Reading the Orchestrator settings…</p>
      </div>
    );
  }
  const dirty = !saved || JSON.stringify([draft, texts]) !== JSON.stringify([saved, textsOf(saved)]);
  const patch = (f: (d: WorkflowSettings) => WorkflowSettings) => {
    setDraft((d) => (d ? f(d) : d));
    setDone(false);
  };
  const text = (k: keyof Texts) => (v: string) => {
    setTexts((t) => (t ? { ...t, [k]: v } : t));
    setDone(false);
  };
  const save = async () => {
    const a = assemble(draft, texts);
    if ('error' in a) {
      setError(a.error);
      return;
    }
    setBusy(true);
    setError(null);
    try {
      const next = await store.ops.settingsSave(a.settings);
      setDraft(next);
      setTexts(textsOf(next));
      await store.loadSettings();
      setDone(true);
    } catch (e) {
      setError(errorText(e));
    } finally {
      setBusy(false);
    }
  };
  const revert = () => {
    if (!saved) return;
    setDraft(saved);
    setTexts(textsOf(saved));
    setError(null);
    setDone(false);
  };
  const st = draft.stages;
  const setStages = (f: (s: WorkflowSettings['stages']) => WorkflowSettings['stages']) => patch((d) => ({ ...d, stages: f(d.stages) }));

  return (
    <div className="orch-pane" data-testid="orchestrator-pane">
      <p className="orch-intro">
        The staged workflow for Claude Code chats: plan, review, build, review, clean-up and land. Only you can change these settings.
      </p>

      <SettingRow title="Orchestrator workflow" description="Turn the workflow on. Refused while a Claude Code chat is running a turn." testId="orch-enabled-row">
        <Checkbox checked={draft.enabled} label={draft.enabled ? 'On' : 'Off'} onChange={(v) => patch((d) => ({ ...d, enabled: v }))} data-testid="orch-enabled" />
      </SettingRow>

      <SettingRow
        title="Orchestrator for new chats"
        description="The state a chat starts with. Always: on. Never: off. Auto: off until the chat's agent turns it on for a task. Chats already used keep the state they had."
        testId="orch-newchats-row"
      >
        <Segmented<NewChats>
          label="Orchestrator for new chats"
          testId="orch-newchats"
          value={draft.newChats}
          onChange={(v) => patch((d) => ({ ...d, newChats: v }))}
          options={[
            { value: 'always', label: 'Always' },
            { value: 'never', label: 'Never' },
            { value: 'auto', label: 'Auto' },
          ]}
        />
      </SettingRow>

      <SettingRow
        title="Move fast"
        description="Adds one fixed paragraph to the workflow rules: coders and reviewers test only what a change touches, and full builds wait for the last PR of a series or a periodic full check. It changes testing depth only, never plans, reviews or your consent. Not Claude Code's Fast mode."
        testId="orch-movefast-row"
      >
        <Checkbox checked={draft.moveFast} label={draft.moveFast ? 'On' : 'Off'} onChange={(v) => patch((d) => ({ ...d, moveFast: v }))} data-testid="orch-movefast" />
      </SettingRow>

      <h3 className="orch-h">Stages</h3>
      <div className="orch-stages" data-testid="orch-stages">
        <StageLine
          id="plan"
          title="Plan"
          use={st.plan.use}
          options={USE_ALL}
          agent={st.plan.agent}
          onUse={(u) => setStages((s) => ({ ...s, plan: { ...s.plan, use: u } }))}
          onAgent={(a) => setStages((s) => ({ ...s, plan: { ...s.plan, agent: a } }))}
        />
        <StageLine
          id="planReview"
          title="Plan review"
          use={st.planReview.use}
          options={USE_ALL}
          agent={st.planReview.agent}
          rounds={st.planReview.maxRounds}
          onUse={(u) => setStages((s) => ({ ...s, planReview: { ...s.planReview, use: u } }))}
          onAgent={(a) => setStages((s) => ({ ...s, planReview: { ...s.planReview, agent: a } }))}
          onRounds={(n) => setStages((s) => ({ ...s, planReview: { ...s.planReview, maxRounds: n } }))}
        />
        <div className="orch-stage" data-stage="build">
          <div className="orch-stage-title">Build</div>
          <span className="orch-fixed">Always</span>
          <TextInput aria-label="Build agent" value={st.build.agent} onChange={(e) => setStages((s) => ({ ...s, build: { ...s.build, agent: e.target.value } }))} />
        </div>
        <StageLine
          id="review"
          title="Review"
          use={st.review.use}
          options={USE_REVIEW}
          agent={st.review.agent}
          rounds={st.review.maxRounds}
          onUse={(u) => setStages((s) => ({ ...s, review: { ...s.review, use: u } }))}
          onAgent={(a) => setStages((s) => ({ ...s, review: { ...s.review, agent: a } }))}
          onRounds={(n) => setStages((s) => ({ ...s, review: { ...s.review, maxRounds: n } }))}
        />
        <StageLine
          id="cleanup"
          title="Clean-up"
          use={st.cleanup.use}
          options={USE_ALL}
          agent={st.cleanup.agent}
          onUse={(u) => setStages((s) => ({ ...s, cleanup: { ...s.cleanup, use: u } }))}
          onAgent={(a) => setStages((s) => ({ ...s, cleanup: { ...s.cleanup, agent: a } }))}
        />
        <StageLine
          id="smoketest"
          title="Smoketest"
          use={st.smoketest.use}
          options={USE_ALL}
          agent={st.smoketest.agent}
          rounds={st.smoketest.maxRounds}
          onUse={(u) => setStages((s) => ({ ...s, smoketest: { ...s.smoketest, use: u } }))}
          onAgent={(a) => setStages((s) => ({ ...s, smoketest: { ...s.smoketest, agent: a } }))}
          onRounds={(n) => setStages((s) => ({ ...s, smoketest: { ...s.smoketest, maxRounds: n } }))}
        />
      </div>
      <p className="orch-note">A task marked visual needs a smoketest whatever this says. Review has no Auto, so only you can skip it.</p>

      <SettingRow title="Agents that start a task" description="Names, separated by commas. Dispatching one outside a task starts one." testId="orch-start-row">
        <TextInput aria-label="Agents that start a task" value={texts.startAgents} onChange={(e) => text('startAgents')(e.target.value)} />
      </SettingRow>
      <SettingRow title="Agents that are not a build" description="Names, separated by commas. Dispatching one during a task is not a build." testId="orch-exempt-row">
        <TextInput aria-label="Agents that are not a build" value={texts.buildExempt} onChange={(e) => text('buildExempt')(e.target.value)} />
      </SettingRow>

      <SettingRow title="Orchestrator edits" description="Whether the orchestrator may edit files in a task's repositories itself." testId="orch-edits-row">
        <Segmented
          label="Orchestrator edits"
          testId="orch-edits"
          value={draft.orchestratorEdits}
          onChange={(v) => patch((d) => ({ ...d, orchestratorEdits: v }))}
          options={[
            { value: 'deny', label: 'Deny' },
            { value: 'allow', label: 'Allow' },
          ]}
        />
      </SettingRow>
      <SettingRow title="Plan sign-off" description="Plan review: an approving review is enough. Me: you also sign off every plan before any build." testId="orch-signoff-row">
        <Segmented
          label="Plan sign-off"
          testId="orch-signoff"
          value={draft.planSignOff}
          onChange={(v) => patch((d) => ({ ...d, planSignOff: v }))}
          options={[
            { value: 'review', label: 'Plan review' },
            { value: 'me', label: 'Me' },
          ]}
        />
      </SettingRow>
      <SettingRow title="Home repositories" description="One host/owner/name per line. Working in one of these does not make it part of every task." testId="orch-home-row">
        <TextArea aria-label="Home repositories" rows={3} value={texts.homeRepos} onChange={(e) => text('homeRepos')(e.target.value)} />
      </SettingRow>

      <h3 className="orch-h">Landing</h3>
      <SettingRow title="Landing default" description="How a repository with no rule of its own lands." testId="orch-landdefault-row">
        <Segmented<LandMode>
          label="Landing default"
          testId="orch-landdefault"
          value={draft.landing.default}
          onChange={(v) => patch((d) => ({ ...d, landing: { ...d.landing, default: v } }))}
          options={[
            { value: 'draft', label: 'Draft PR' },
            { value: 'merge', label: 'Merge' },
          ]}
        />
      </SettingRow>
      <SettingRow title="Merge method" testId="orch-method-row">
        <Segmented<MergeMethod>
          label="Merge method"
          testId="orch-method"
          value={draft.landing.method}
          onChange={(v) => patch((d) => ({ ...d, landing: { ...d.landing, method: v } }))}
          options={[
            { value: 'merge', label: 'Merge' },
            { value: 'squash', label: 'Squash' },
            { value: 'rebase', label: 'Rebase' },
          ]}
        />
      </SettingRow>
      <SettingRow title="Repository rules" description="One per line: host/owner/name draft, or host/owner/name merge [merge|squash|rebase]. A repository may appear once." testId="orch-rules-row">
        <TextArea aria-label="Repository rules" rows={3} value={texts.rules} onChange={(e) => text('rules')(e.target.value)} />
      </SettingRow>
      <SettingRow title="Draft labels" description="Up to 8, separated by commas. Added to a PR at draft hand-off." testId="orch-labels-row">
        <TextInput aria-label="Draft labels" value={texts.draftLabels} onChange={(e) => text('draftLabels')(e.target.value)} />
      </SettingRow>
      <SettingRow title="Ready-for-review label" description="Removed when you press Ready for review. Leave empty for none." testId="orch-flip-row">
        <TextInput aria-label="Ready-for-review label" value={texts.flipLabel} onChange={(e) => text('flipLabel')(e.target.value)} />
      </SettingRow>
      <SettingRow title="Protected branches" description="The only branches the land step merges into, and branches agents may not land on. Empty means it merges nothing." testId="orch-protected-row">
        <TextInput aria-label="Protected branches" value={texts.protectedBranches} onChange={(e) => text('protectedBranches')(e.target.value)} />
      </SettingRow>

      <div className="orch-actions">
        <Button variant="primary" disabled={!dirty || busy} onClick={() => void save()} data-testid="orch-save">
          Save
        </Button>
        <Button variant="secondary" disabled={!dirty || busy} onClick={revert} data-testid="orch-revert">
          Revert
        </Button>
        {done && !dirty && (
          <span className="orch-saved" data-testid="orch-saved">
            Saved
          </span>
        )}
        {error && (
          <span className="orch-save-error" role="alert" data-testid="orch-error">
            {error}
          </span>
        )}
      </div>
    </div>
  );
}
