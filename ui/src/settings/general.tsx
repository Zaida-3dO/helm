// Settings → General: appearance (System / Light / Dark), text sizes and zoom, and compact sidebar. Each control saves
// at once through the generic `preference` op and applies at once.

import { useEffect, useState } from 'react';
import { useShell } from '../shell/context.ts';
import { FONT_DEFAULTS, parseAppearance, parseFontSizes, parseZoom, ZOOM_DEFAULT, type Appearance, type FontSizes } from '../shell/theme.ts';
import { Checkbox, ChoiceTiles, Picker, SettingRow, toast } from '../ui/index.ts';
import { IconMonitor, IconMoon, IconSun } from '../ui/icons.tsx';
import { sizeOptions, zoomOptions } from './generalOptions.ts';
import { registerSettingsPane } from './registry.ts';
import { ChangedByChats } from './changed/ChangedByChats.tsx';
import { parseNameList, SIDEBAR_ENTRIES } from './applied.ts';

function GeneralPane() {
  const shell = useShell();
  const [appearance, setAppearance] = useState<Appearance>('system');
  const [compact, setCompact] = useState(false);
  const [sizes, setSizes] = useState<FontSizes>(FONT_DEFAULTS);
  const [zoom, setZoom] = useState(ZOOM_DEFAULT);
  const [allSettings, setAllSettings] = useState(false);
  const [hiddenItems, setHiddenItems] = useState<string[]>([]);
  useEffect(() => {
    shell
      .api<unknown>('preference_get', { key: 'fontSizes' })
      .then((v) => setSizes(parseFontSizes(v)))
      .catch(() => undefined);
    shell
      .api<unknown>('preference_get', { key: 'zoomLevel' })
      .then((v) => setZoom(parseZoom(v)))
      .catch(() => undefined);
    shell
      .api<unknown>('preference_get', { key: 'appearance' })
      .then((v) => setAppearance(parseAppearance(v)))
      .catch(() => undefined);
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
  const setSize = (k: keyof FontSizes, n: number) => {
    const before = sizes;
    const next = { ...sizes, [k]: n };
    setSizes(next);
    apply('fontSizes', next);
    void save('fontSizes', next, () => {
      setSizes(before);
      apply('fontSizes', before);
    });
  };
  return (
    <div>
      <SettingRow title="Appearance" description="Follow Windows, or always use the light or dark colours." testId="setting-appearance">
        <ChoiceTiles<Appearance>
          label="Appearance"
          testId="appearance"
          value={appearance}
          options={[
            { value: 'system', label: 'System', icon: <IconMonitor size={17} /> },
            { value: 'light', label: 'Light', icon: <IconSun size={17} /> },
            { value: 'dark', label: 'Dark', icon: <IconMoon size={17} /> },
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
      <SettingRow title="Interface text size" description="The font size of menus, the sidebar and every control. The whole interface scales with it." testId="setting-interface-size">
        <Picker<string>
          settings
          label="Interface text size"
          testId="interface-size"
          value={String(sizes.interface)}
          options={sizeOptions('interface')}
          onChange={(v) => setSize('interface', Number(v))}
        />
      </SettingRow>
      <SettingRow title="Reply text size" description="The font size of Claude's replies in the chat." testId="setting-response-size">
        <Picker<string>
          settings
          label="Reply text size"
          testId="response-size"
          value={String(sizes.response)}
          options={sizeOptions('response')}
          onChange={(v) => setSize('response', Number(v))}
        />
      </SettingRow>
      <SettingRow title="Zoom" description="Make everything in the window larger or smaller." testId="setting-zoom">
        <Picker<string>
          settings
          label="Zoom"
          testId="zoom"
          value={String(zoom)}
          options={zoomOptions()}
          onChange={(v) => {
            const before = zoom;
            const next = Number(v);
            setZoom(next);
            apply('zoomLevel', next);
            void save('zoomLevel', next, () => {
              setZoom(before);
              apply('zoomLevel', before);
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
      <ChangedByChats />
    </div>
  );
}

registerSettingsPane({ id: 'general', title: 'General', order: 0, render: () => <GeneralPane /> });
