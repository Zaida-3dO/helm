// Settings → Usage governor, second half: how many chats may run at once before an automated
// start has to wait for a free slot. These save as soon as they change (they are not part of the
// governor's Save).

import { useState } from 'react';
import { MAX_SLOTS, MIN_SLOTS, slotQueueOps, type SlotSettings } from '../composer/slotQueue.ts';
import { useSlotQueue } from '../composer/useSlotQueue.ts';
import { useShell } from '../shell/context.ts';
import { Checkbox, Picker, SettingRow, toast, type PickerOption } from '../ui/index.ts';

const COUNT_OPTIONS: PickerOption<string>[] = Array.from({ length: MAX_SLOTS - MIN_SLOTS + 1 }, (_, i) => {
  const n = String(MIN_SLOTS + i);
  return { value: n, title: n };
});

export function RunSlotsSection() {
  const shell = useShell();
  const { view, store } = useSlotQueue();
  const [busy, setBusy] = useState(false);

  const save = (change: Partial<SlotSettings>) => {
    setBusy(true);
    slotQueueOps(shell.api)
      .saveSettings(change)
      .then((v) => store.adopt(v))
      .catch((e: unknown) => toast.show({ kind: 'error', text: String((e as Error)?.message ?? e) }))
      .finally(() => setBusy(false));
  };

  const s = view?.settings;
  return (
    <div data-testid="run-slots">
      <h3 className="afk-heading">Run slots</h3>
      {view && (
        <p className="afk-notice" data-testid="run-slots-state">
          {view.running.length} of {view.limit} running
          {view.waiting.length > 0 ? `, ${view.waiting.length} waiting for a slot` : ''}
          {view.held ? '. Automated starts are on hold.' : '.'}
        </p>
      )}
      <SettingRow
        title="Limit automated starts"
        description="When every slot is taken, a message from another chat, a scheduled prompt or an agent’s first message to a new chat waits for a free slot. Your own messages never wait."
        testId="run-slots-enabled"
      >
        <Checkbox checked={s?.enabled ?? true} disabled={!s || busy} onChange={(on) => save({ enabled: on })} data-testid="run-slots-enabled-box" />
      </SettingRow>
      <SettingRow title="Chats running at once" description={`Between ${MIN_SLOTS} and ${MAX_SLOTS}. Start now on a waiting message ignores the limit.`}>
        <Picker
          settings
          label="Chats running at once"
          testId="run-slots-count"
          value={s ? String(s.lowRunningChats) : null}
          options={COUNT_OPTIONS}
          disabled={!s || busy || !s.enabled}
          onChange={(v: string) => save({ lowRunningChats: Number(v) })}
        />
      </SettingRow>
    </div>
  );
}
