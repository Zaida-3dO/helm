// The settings page edits its list fields as text. These turn a settings object into those texts and
// back (pure, so node:test can run them).

import type { WorkflowSettings } from './api.ts';
import { listFromText, listToText, rulesFromText, rulesToText } from './model.ts';

export interface Texts {
  startAgents: string;
  buildExempt: string;
  homeRepos: string;
  rules: string;
  draftLabels: string;
  flipLabel: string;
  protectedBranches: string;
}

export function textsOf(s: WorkflowSettings): Texts {
  return {
    startAgents: listToText(s.stages.build.startAgents),
    buildExempt: listToText(s.stages.build.buildExempt),
    homeRepos: s.homeRepos.join('\n'),
    rules: rulesToText(s.landing.repos),
    draftLabels: listToText(s.landing.draftLabels),
    flipLabel: s.landing.flipLabel ?? '',
    protectedBranches: listToText(s.landing.protectedBranches),
  };
}

/** The settings the draft and its texts describe, or why they cannot be saved yet. */
export function assemble(draft: WorkflowSettings, t: Texts): { settings: WorkflowSettings } | { error: string } {
  const rules = rulesFromText(t.rules);
  if ('error' in rules) return { error: rules.error };
  return {
    settings: {
      ...draft,
      stages: {
        ...draft.stages,
        build: { ...draft.stages.build, startAgents: listFromText(t.startAgents), buildExempt: listFromText(t.buildExempt) },
      },
      homeRepos: listFromText(t.homeRepos),
      landing: {
        ...draft.landing,
        repos: rules.rules,
        draftLabels: listFromText(t.draftLabels),
        flipLabel: t.flipLabel.trim() || null,
        protectedBranches: listFromText(t.protectedBranches),
      },
    },
  };
}
