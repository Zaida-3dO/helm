// Settings → General: the compact sidebar and which sidebar entries show, whether chats may change
// every setting, and where updates come from. How Helm looks, moves and sounds lives in the
// Appearance pane. Each control saves at once through the generic `preference` op and applies at once.

import { useEffect, useState } from 'react';
import { useShell } from '../shell/context.ts';
import { Checkbox, SettingRow, toast } from '../ui/index.ts';
import { registerSettingsPane } from './registry.ts';
import { ChangedByChats } from './changed/ChangedByChats.tsx';
import { parseNameList, SIDEBAR_ENTRIES } from './applied.ts';
import { UpdateSourceRow } from '../updates/UpdateSourceRow.tsx';

function GeneralPane() {
  const shell = useShell();
  const [compact, setCompact] = useState(false);
  const [allSettings, setAllSettings] = useState(false);
  const [hiddenItems, setHiddenItems] = useState<string[]>([]);
  useEffect(() => {
    shell
      .api<unknown>('preference_get', { key: 'agentSettingsAll' })
      .then((v) => setAllSettings(v === true))
      .catch(() => undefined);
    shell
      .api<unknown>('preference_get', { key: 'hiddenSidebarItems' })
      .then((v) => setHiddenItems(parseNameList(v)))
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
  // Apply at once through the same event the host sends after a save; undo on a failed save.
  const apply = (key: string, value: unknown) => shell.bus.dispatch({ kind: 'preferences', changed: { [key]: value } });
  return (
    <div>
      <SettingRow title="Compact sidebar" description="Tighter rows in the sidebar.">
        <Checkbox
          checked={compact}
          onChange={(on) => {
            setCompact(on);
            void save('compactSidebar', on, () => setCompact(!on));
          }}
        />
      </SettingRow>
      {SIDEBAR_ENTRIES.map((e) => (
        <SettingRow key={e.id} title={`Show ${e.title} in the sidebar`} description={e.description} testId={`setting-sidebar-${e.id}`}>
          <Checkbox
            checked={!hiddenItems.includes(e.id)}
            onChange={(show) => {
              const before = hiddenItems;
              const next = show ? before.filter((x) => x !== e.id) : [...before, e.id];
              setHiddenItems(next);
              apply('hiddenSidebarItems', next);
              void save('hiddenSidebarItems', next, () => {
                setHiddenItems(before);
                apply('hiddenSidebarItems', before);
              });
            }}
          />
        </SettingRow>
      ))}
      <SettingRow
        title="Chats can change all settings"
        description="Chats can already change most settings and you can undo each change below. Turn this on to let them change every setting, including your shortcuts."
        testId="setting-agent-all"
      >
        <Checkbox
          checked={allSettings}
          onChange={(on) => {
            setAllSettings(on);
            shell.api('settings_all_set', { enabled: on }).catch((e: unknown) => {
              setAllSettings(!on);
              toast.show({ kind: 'error', text: `Could not save: ${String((e as Error).message ?? e)}` });
            });
          }}
        />
      </SettingRow>
      <UpdateSourceRow />
      <ChangedByChats />
    </div>
  );
}

registerSettingsPane({ id: 'general', title: 'General', order: 0, render: () => <GeneralPane /> });
