// Settings → Appearance: how Helm looks, moves and sounds. Each control saves at once through the
// generic `preference` op and applies at once (usePref). The pane is a list of sections; other
// areas add their own with `registerAppearanceSection` (the desktop pet puts its switches here)
// without editing this file.

import { Fragment, useSyncExternalStore, type ReactNode } from 'react';
import { parseAppearance, parseDopamine, parseFontSizes, parseSkin, parseZoom, type Appearance, type Dopamine, type FontSizes, type Skin } from '../../shell/theme.ts';
import { parseSounds, type Sounds } from '../../shell/sounds.ts';
import { Checkbox, ChoiceTiles, Picker, SettingRow } from '../../ui/index.ts';
import { IconMonitor, IconMoon, IconSun } from '../../ui/icons.tsx';
import { motionOptions, sizeOptions, skinOptions, volumeOptions, zoomOptions } from '../generalOptions.ts';
import { registerSettingsPane } from '../registry.ts';
import { ColoursRow } from './ColoursRow.tsx';
import { usePref } from './usePref.ts';
import './appearance.css';

// --- sections registry ------------------------------------------------------------------------

interface AppearanceSectionEntry {
  id: string;
  order: number;
  render: () => ReactNode;
}

const sections = new Map<string, AppearanceSectionEntry>();
const listeners = new Set<() => void>();
let snapshot: AppearanceSectionEntry[] = [];

function changed(): void {
  snapshot = [...sections.values()].sort((a, b) => a.order - b.order || a.id.localeCompare(b.id));
  for (const l of [...listeners]) l();
}

/**
 * Add (or replace) a section of the Appearance pane. `render` returns the whole section, usually an
 * `AppearanceSection`. The built-in sections sit at 10 (Look), 20 (Motion), 30 (Sound) and 40
 * (Text and zoom); the desktop pet's goes after them, at 50. Returns the remove function.
 */
export function registerAppearanceSection(id: string, order: number, render: () => ReactNode): () => void {
  const entry = { id, order, render };
  sections.set(id, entry);
  changed();
  return () => {
    if (sections.get(id) === entry) {
      sections.delete(id);
      changed();
    }
  };
}

/** The sections now registered, in display order (a stable array between changes). */
export function appearanceSections(): readonly AppearanceSectionEntry[] {
  return snapshot;
}

/** A titled group of rows. */
export function AppearanceSection({ id, title, children }: { id: string; title: string; children: ReactNode }) {
  return (
    <div className="appearance-section" data-testid={`appearance-section-${id}`}>
      <h4 className="settings-section">{title}</h4>
      {children}
    </div>
  );
}

// --- built-in sections ------------------------------------------------------------------------

const parseBool = (v: unknown) => v === true;

function LookSection() {
  const [appearance, setAppearance] = usePref<Appearance>('appearance', parseAppearance);
  const [skin, setSkin] = usePref<Skin>('interfaceTheme', parseSkin);
  return (
    <AppearanceSection id="look" title="Look">
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
          onChange={setAppearance}
        />
      </SettingRow>
      <SettingRow title="Theme" description="Recolour the whole app. Every theme comes in a light and dark form; Appearance above picks which of the two it shows in." testId="setting-theme">
        <Picker<string> settings label="Theme" testId="theme" value={skin} options={skinOptions()} onChange={(v) => setSkin(v as Skin)} />
      </SettingRow>
      <ColoursRow />
    </AppearanceSection>
  );
}

function MotionSection() {
  const [motion, setMotion] = usePref<Dopamine>('dopamine', parseDopamine);
  const [pause, setPause] = usePref<boolean>('pauseAnimationsOnScroll', parseBool);
  return (
    <AppearanceSection id="motion" title="Motion">
      <SettingRow title="Motion and flair" description="How much movement the interface shows. Quiet stills the animations and silences every sound; Maximal leans into them." testId="setting-motion">
        <Picker<string> settings label="Motion and flair" testId="motion" value={motion} options={motionOptions()} onChange={(v) => setMotion(v as Dopamine)} />
      </SettingRow>
      <SettingRow title="Pause animations while scrolling" description="Hold decorative animations still while a list or the chat is scrolling, so scrolling stays smooth." testId="setting-pause-scroll">
        <Checkbox data-testid="pause-scroll" checked={pause} onChange={setPause} />
      </SettingRow>
    </AppearanceSection>
  );
}

function SoundSection() {
  const [sounds, setSounds] = usePref<Sounds>('sounds', parseSounds);
  const patch = (p: Partial<Sounds>) => setSounds({ ...sounds, ...p });
  return (
    <AppearanceSection id="sound" title="Sound">
      <SettingRow title="Interface sounds" description="A quiet chime when a chat finishes while the window is away, and a softer pair of notes when a chat turns up needing you." testId="setting-sounds">
        <Checkbox data-testid="sounds-enabled" checked={sounds.enabled} onChange={(enabled) => patch({ enabled })} />
      </SettingRow>
      <SettingRow title="Sound level" description="How loud the sounds are." testId="setting-sound-level">
        <Picker<string> settings label="Sound level" testId="sound-level" value={String(sounds.volume)} options={volumeOptions()} onChange={(v) => patch({ volume: Number(v) })} />
      </SettingRow>
      <SettingRow title="Small interface sounds" description="A soft whoosh when you send a message and a tick on small actions. Off until you want them." testId="setting-sound-cues">
        <Checkbox data-testid="sound-cues" checked={sounds.cues} onChange={(cues) => patch({ cues })} />
      </SettingRow>
      <SettingRow title="Play even when Helm is in front" description="Chimes normally wait until Helm is behind another window. Turn this on to hear them with Helm in front too." testId="setting-sound-focused">
        <Checkbox data-testid="sound-focused" checked={sounds.focused} onChange={(focused) => patch({ focused })} />
      </SettingRow>
    </AppearanceSection>
  );
}

function TextSection() {
  const [sizes, setSizes] = usePref<FontSizes>('fontSizes', parseFontSizes);
  const [zoom, setZoom] = usePref<number>('zoomLevel', parseZoom);
  return (
    <AppearanceSection id="text" title="Text and zoom">
      <SettingRow title="Interface text size" description="The font size of menus, the sidebar and every control. The whole interface scales with it." testId="setting-interface-size">
        <Picker<string> settings label="Interface text size" testId="interface-size" value={String(sizes.interface)} options={sizeOptions('interface')} onChange={(v) => setSizes({ ...sizes, interface: Number(v) })} />
      </SettingRow>
      <SettingRow title="Reply text size" description="The font size of Claude's replies in the chat." testId="setting-response-size">
        <Picker<string> settings label="Reply text size" testId="response-size" value={String(sizes.response)} options={sizeOptions('response')} onChange={(v) => setSizes({ ...sizes, response: Number(v) })} />
      </SettingRow>
      <SettingRow title="Zoom" description="Make everything in the window larger or smaller." testId="setting-zoom">
        <Picker<string> settings label="Zoom" testId="zoom" value={String(zoom)} options={zoomOptions()} onChange={(v) => setZoom(Number(v))} />
      </SettingRow>
    </AppearanceSection>
  );
}

registerAppearanceSection('look', 10, () => <LookSection />);
registerAppearanceSection('motion', 20, () => <MotionSection />);
registerAppearanceSection('sound', 30, () => <SoundSection />);
registerAppearanceSection('text', 40, () => <TextSection />);
// Section 50, "Desktop pet", is added by the pet's own module; until it is, the pane ends here.

function subscribe(l: () => void): () => void {
  listeners.add(l);
  return () => {
    listeners.delete(l);
  };
}

function AppearancePane() {
  const list = useSyncExternalStore(subscribe, appearanceSections);
  return (
    <div data-testid="appearance-pane">
      {list.map((s) => (
        <Fragment key={s.id}>{s.render()}</Fragment>
      ))}
    </div>
  );
}

registerSettingsPane({ id: 'appearance', title: 'Appearance', order: 2, render: () => <AppearancePane /> });
