import assert from 'node:assert/strict';
import { test } from 'node:test';
import type { WorkModeView } from '../api/generated/gatekeeper/index.ts';
import { boardChoiceLabel, chipTooltip, chipWord, choiceToMode, countsLine, modeLabel, modeToChoice } from './model.ts';

const view = (over: Partial<WorkModeView> = {}): WorkModeView => ({
  app: 'check_in',
  effective: 'check_in',
  source: 'app',
  overrides: [],
  counts: { decided: 0, needsYou: 0 },
  ...over,
});

test("the chip, its tooltip and the popover's wording, per mode, tier and source", () => {
  for (const [got, want] of [
    [modeLabel('run'), 'Run to completion'],
    [modeLabel('check_in'), 'Check in with me'],
    // The word from `mid` up; the glyph alone at the bar's narrow and tight tiers.
    [chipWord('run', 'wide'), 'Run'],
    [chipWord('check_in', 'wide'), 'Check in'],
    [chipWord('run', 'mid'), 'Run'],
    [chipWord('check_in', 'mid'), 'Check in'],
    [chipWord('run', 'narrow'), ''],
    [chipWord('check_in', 'narrow'), ''],
    [chipWord('run', 'tight'), ''],
    [chipWord('check_in', 'tight'), ''],
    // The tooltip says where the mode was set.
    [chipTooltip(view({ app: 'run', effective: 'run' })), 'Run to completion — set in Helm for every chat'],
    [
      chipTooltip(view({ app: 'run', effective: 'check_in', source: 'board', board: { id: 'b1', name: 'Garden', mode: 'check_in' } })),
      "Check in with me — this board's own choice (Garden)",
    ],
    // A board source with no board row (a cleared list read late) still reads as the app's.
    [chipTooltip(view({ source: 'board' })), 'Check in with me — set in Helm for every chat'],
    [countsLine({ decided: 0, needsYou: 0 }), '0 decided for you · 0 need you'],
    [countsLine({ decided: 3, needsYou: 12 }), '3 decided for you · 12 need you'],
    [countsLine({ decided: 1, needsYou: 1 }), '1 decided for you · 1 needs you'],
    [boardChoiceLabel(null), 'Same as app'],
    [boardChoiceLabel('run'), 'Run to completion'],
    [boardChoiceLabel('check_in'), 'Check in with me'],
    // The three board picks map through the mode ids.
    [choiceToMode('same'), null],
    [choiceToMode('run'), 'run'],
    [choiceToMode('check_in'), 'check_in'],
    [modeToChoice(null), 'same'],
    [modeToChoice('run'), 'run'],
    [modeToChoice('check_in'), 'check_in'],
  ] as const)
    assert.equal(got, want);
});
