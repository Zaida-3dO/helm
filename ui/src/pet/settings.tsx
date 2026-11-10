// Settings → Appearance → Desktop pet. Adds its section to the pane through the pane's registry,
// so the Appearance pane itself does not change. Every control saves at once (usePref).

import { AppearanceSection, registerAppearanceSection } from '../settings/index.ts';
import { usePref } from '../settings/appearance/usePref.ts';
import { Checkbox, Segmented, SettingRow } from '../ui/index.ts';
import { Creature, useMayMove } from './Creature.tsx';
import { parsePet, PET_KINDS, type PetKind, type PetPref, type PetSize } from './model.ts';
import './pet.css';

const NAMES: Record<PetKind, { name: string; what: string }> = {
  puffin: { name: 'Pip', what: 'puffin' },
  crab: { name: 'Dock', what: 'hermit crab' },
  lantern: { name: 'Glim', what: 'lantern' },
};

/** What is stored: the choices, and the spot the window was left at (the host keeps that one). */
export function petToStored(p: PetPref): unknown {
  const { enabled, kind, size, bubble, position } = p;
  return { enabled, kind, size, bubble, ...(position ? { position } : {}) };
}

function PetSection() {
  const [pet, setPet] = usePref<PetPref>('desktopPet', parsePet, petToStored);
  const moving = useMayMove();
  const patch = (p: Partial<PetPref>) => setPet({ ...pet, ...p });
  return (
    <AppearanceSection id="pet" title="Desktop pet">
      <SettingRow
        title="Show pet"
        description="A small companion that sits on your desktop and reacts to your chats. Click it to jump to what needs you. It rests when motion is reduced."
        testId="setting-pet"
      >
        <Checkbox data-testid="pet-toggle" checked={pet.enabled} onChange={(enabled) => patch({ enabled })} />
      </SettingRow>
      <SettingRow title="Companion" description="Pip the puffin, Dock the hermit crab or Glim the lantern." testId="setting-pet-kind">
        <div className="pet-cards" role="radiogroup" aria-label="Companion" data-pet-motion={moving ? 'on' : 'off'}>
          {PET_KINDS.map((kind) => (
            <button
              key={kind}
              type="button"
              role="radio"
              aria-checked={pet.kind === kind}
              className="ui-tile pet-card"
              data-testid={`pet-kind-${kind}`}
              onClick={() => patch({ kind })}
            >
              <span className="pet-preview">
                <Creature kind={kind} pose="idle" blink={false} />
              </span>
              <span>
                {NAMES[kind].name}, the {NAMES[kind].what}
              </span>
            </button>
          ))}
        </div>
      </SettingRow>
      <SettingRow title="Size" description="How big the companion is on the desktop." testId="setting-pet-size">
        <Segmented<PetSize>
          label="Size"
          testId="pet-size"
          value={pet.size}
          options={[
            { value: 's', label: 'Small' },
            { value: 'm', label: 'Medium' },
            { value: 'l', label: 'Large' },
          ]}
          onChange={(size) => patch({ size })}
        />
      </SettingRow>
      <SettingRow title="Speech bubble" description="Say how many chats need you, or have finished, above the companion." testId="setting-pet-bubble">
        <Checkbox data-testid="pet-bubble-toggle" checked={pet.bubble} onChange={(bubble) => patch({ bubble })} />
      </SettingRow>
    </AppearanceSection>
  );
}

registerAppearanceSection('pet', 50, () => <PetSection />);
