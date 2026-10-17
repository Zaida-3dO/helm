// Settings → General: appearance (System / Light / Dark) and compact sidebar. Each
// control saves at once through the generic `preference` op. Font sizes come later (follow-up).

import { useEffect, useState } from 'react';
import { useShell } from '../shell/context.ts';
import { parseAppearance, type Appearance } from '../shell/theme.ts';
import { Checkbox, Segmented, SettingRow, toast } from '../ui/index.ts';
import { registerSettingsPane } from './registry.ts';

function GeneralPane() {
  const shell = useShell();
  const [appearance, setAppearance] = useState<Appearance>('system');
  const [compact, setCompact] = useState(false);
  useEffect(() => {
    shell
      .api<unknown>('preference_get', { key: 'appearance' })
      .then((v) => setAppearance(parseAppearance(v)))
      .catch(() => undefined);
    shell
      .api<unknown>('preference_get', { key: 'compactSidebar' })
      .then((v) => setCompact(v === true))
      .catch(() => undefined);
  }, [shell]);
  const save = (key: string, value: unknown, undo: () => void) =>
    shell.api('preference', { key, value }).catch((e: unknown) => {
      undo();
      toast.show({ kind: 'error', text: `Could not save: ${String((e as Error).message ?? e)}` });
    });
  return (
    <div>
      <SettingRow title="Appearance" description="Follow Windows, or always use the light or dark colours." testId="setting-appearance">
        <Segmented<Appearance>
          label="Appearance"
          testId="appearance"
          value={appearance}
          options={[
            { value: 'system', label: 'System' },
            { value: 'light', label: 'Light' },
            { value: 'dark', label: 'Dark' },
          ]}
          onChange={(v) => {
            const before = appearance;
            setAppearance(v);
            // Apply at once; the host's own `preferences` event re-applies the same value.
            shell.bus.dispatch({ kind: 'preferences', changed: { appearance: v } });
            void save('appearance', v, () => {
              setAppearance(before);
              shell.bus.dispatch({ kind: 'preferences', changed: { appearance: before } });
            });
          }}
        />
      </SettingRow>
      <SettingRow title="Compact sidebar" description="Tighter rows in the sidebar.">
        <Checkbox
          checked={compact}
          onChange={(on) => {
            setCompact(on);
            void save('compactSidebar', on, () => setCompact(!on));
          }}
        />
      </SettingRow>
    </div>
  );
}

registerSettingsPane({ id: 'general', title: 'General', order: 0, render: () => <GeneralPane /> });
