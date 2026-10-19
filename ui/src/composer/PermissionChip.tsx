// The permission-mode chip: a compact
// picker for Bypass permissions, Asks permission, Accept edits and Plan mode. A pick goes to the
// host, which tells a live Claude Code (`set_permission_mode`) before its next turn and gives it to
// the chat's next worker at start. The chip shows the pick until the CLI reports it live.

import { useEffect, useRef, useState } from 'react';
import type { ChatOps } from '../api/ops.ts';
import { Picker } from '../ui/Picker.tsx';
import { IconPlan, IconShield, IconShieldOff } from '../ui/icons.tsx';
import { afterTurn, newPick, PICKABLE_MODES, permissionLabel, permissionTooltip, pickedMode, setPickedMode, shownMode, type Pick } from './permission.ts';

function ModeIcon({ mode }: { mode: string }) {
  if (mode === 'bypassPermissions') return <IconShieldOff size={11} />;
  if (mode === 'plan') return <IconPlan size={11} />;
  return <IconShield size={11} />;
}

export function PermissionChip({ chatId, mode: live, busy, ops }: { chatId: string; mode: string | null; busy: boolean; ops: ChatOps }) {
  const [picked, setPicked] = useState<Pick | null>(() => pickedMode(chatId));
  const [error, setError] = useState<string | null>(null);
  const { mode, pending } = shownMode(picked?.mode ?? null, live);
  const keep = (p: Pick | null) => {
    setPicked(p);
    setPickedMode(chatId, p);
  };
  // The CLI reported the pick: the live mode speaks for itself from here.
  useEffect(() => {
    if (picked && picked.mode === live) keep(null);
    // eslint-disable-next-line react-hooks/exhaustive-deps
  }, [picked, live, chatId]);
  // At a turn's end, a pick the CLI should have taken by now and did not was refused.
  const wasBusy = useRef(busy);
  useEffect(() => {
    if (wasBusy.current && !busy && picked) {
      const r = afterTurn(picked, live);
      keep(r.pick);
      if (r.refused) setError(`Claude Code kept ${permissionLabel(live)}; ${permissionLabel(picked.mode)} did not apply.`);
    }
    wasBusy.current = busy;
    // eslint-disable-next-line react-hooks/exhaustive-deps
  }, [busy]);

  const pick = (m: string) => {
    if (m === mode) return;
    setError(null);
    keep(newPick(m, busy));
    ops.setPermissionMode(chatId, m).catch((e: unknown) => {
      keep(null);
      setError(String((e as Error).message ?? e));
    });
  };

  return (
    <span
      className={`permission-chip${mode === 'bypassPermissions' ? ' open' : ''}${pending ? ' pending' : ''}`}
      data-testid="permission-chip"
      data-mode={live ?? ''}
      data-picked={pending ? mode : ''}
    >
      <Picker
        compact
        label="Permission mode"
        heading="Permission mode"
        value={mode}
        options={PICKABLE_MODES.map((m) => ({ value: m.value, title: m.title, subtitle: m.detail }))}
        onChange={pick}
        display={
          <span className="permission-chip-label">
            <ModeIcon mode={mode} />
            {permissionLabel(mode)}
          </span>
        }
        title={permissionTooltip(mode, pending)}
        testId="permission-picker"
      />
      {error && (
        <span className="permission-chip-error" role="alert">
          {error}
        </span>
      )}
    </span>
  );
}
