// Settings → Appearance → Colours: your own accent, tint and contrast on top of any skin. The
// colours belong to the profile that is active on this device. Every change shows at once (the
// same `preferences` event the host sends after a save) and is saved after a short pause, so
// dragging the hue slider does not write on every step. A failed save puts the old colours back.

import { useCallback, useEffect, useRef, useState, useSyncExternalStore } from 'react';
import { useShell } from '../../shell/context.ts';
import { COLOUR_PRESETS, COLOURS_DEFAULT, COLOURS_DEFAULT_PROFILE, CUSTOM_COLOURS_PREF, colourProfileKey, coloursAdjusted, coloursFor, isDefaultColours, onColoursAdjusted, withColours, type Colours, type Tint } from '../../shell/colours.ts';
import { Button, Checkbox, Segmented, SettingRow, toast } from '../../ui/index.ts';

const SAVE_DELAY_MS = 300;

/** A hue as the swatches and the slider paint it: a fixed, mid lightness so the row reads at a glance. */
const swatch = (hue: number) => `oklch(0.66 0.16 ${hue})`;
const HUE_TRACK = `linear-gradient(to right, ${[0, 60, 120, 180, 240, 300, 359].map(swatch).join(', ')})`;

export function ColoursRow() {
  const shell = useShell();
  const [map, setMap] = useState<unknown>(null);
  const [profile, setProfile] = useState(COLOURS_DEFAULT_PROFILE);
  const adjusted = useSyncExternalStore(onColoursAdjusted, coloursAdjusted);
  const mapRef = useRef<unknown>(null);
  // The map as it was before the changes that are not saved yet, so a failed save can put it back.
  const unsaved = useRef<{ before: unknown } | null>(null);
  const timer = useRef<ReturnType<typeof setTimeout> | null>(null);

  const colours: Colours = coloursFor(map, profile);

  useEffect(() => {
    let live = true;
    shell
      .api<unknown>('preference_get', { key: CUSTOM_COLOURS_PREF })
      .then((v) => {
        if (!live) return;
        mapRef.current = v;
        setMap(v);
      })
      .catch(() => undefined);
    shell
      .api<unknown>('preference_get', { key: 'activeProfile' })
      .then((v) => live && setProfile(colourProfileKey(v)))
      .catch(() => undefined);
    // A chat can change the colours too, and the profile can switch while Settings is open.
    const off = shell.bus.on('preferences', (e) => {
      const changed = (e.changed ?? {}) as Record<string, unknown>;
      if (CUSTOM_COLOURS_PREF in changed) {
        mapRef.current = changed[CUSTOM_COLOURS_PREF];
        setMap(changed[CUSTOM_COLOURS_PREF]);
      }
      if ('activeProfile' in changed) setProfile(colourProfileKey(changed.activeProfile));
    });
    return () => {
      live = false;
      off();
    };
  }, [shell]);

  const save = useCallback(() => {
    timer.current = null;
    const next = mapRef.current;
    const pending = unsaved.current;
    shell
      .api('preference', { key: CUSTOM_COLOURS_PREF, value: next })
      .then(() => {
        if (unsaved.current === pending) unsaved.current = null;
      })
      .catch((e: unknown) => {
        const before = pending ? pending.before : null;
        unsaved.current = null;
        mapRef.current = before;
        setMap(before);
        shell.bus.dispatch({ kind: 'preferences', changed: { [CUSTOM_COLOURS_PREF]: before } });
        toast.show({ kind: 'error', text: `Could not save: ${String((e as Error).message ?? e)}` });
      });
  }, [shell]);

  // Leaving the pane with a save still waiting sends it.
  useEffect(
    () => () => {
      if (timer.current !== null) {
        clearTimeout(timer.current);
        save();
      }
    },
    [save],
  );

  const change = (patch: Partial<Colours>, delayed = false) => {
    const next = withColours(mapRef.current, profile, { ...coloursFor(mapRef.current, profile), ...patch });
    if (!unsaved.current) unsaved.current = { before: mapRef.current };
    mapRef.current = next;
    setMap(next);
    // Show it now: the interface hears the same event the host sends after a save.
    shell.bus.dispatch({ kind: 'preferences', changed: { [CUSTOM_COLOURS_PREF]: next } });
    if (timer.current !== null) clearTimeout(timer.current);
    if (delayed) timer.current = setTimeout(save, SAVE_DELAY_MS);
    else save();
  };

  const hue = colours.accentHue;
  return (
    <SettingRow
      title="Colours"
      description="Your own accent colour, on top of the theme. It is always adjusted to stay readable, in light and dark. Applies to the profile you are using."
      testId="setting-colours"
    >
      <div className="colours-control" data-testid="colours-control">
        <div className="colour-presets" role="group" aria-label="Accent colour">
          <button
            type="button"
            className="colour-swatch colour-swatch-skin"
            data-testid="colour-preset-skin"
            aria-pressed={hue === null}
            title="Skin's own"
            aria-label="Skin's own"
            onClick={() => change({ accentHue: null })}
          />
          {COLOUR_PRESETS.map((p) => (
            <button
              key={p.id}
              type="button"
              className="colour-swatch"
              data-testid={`colour-preset-${p.id}`}
              aria-pressed={hue === p.hue}
              title={p.title}
              aria-label={p.title}
              style={{ background: swatch(p.hue) }}
              onClick={() => change({ accentHue: p.hue })}
            />
          ))}
        </div>
        <label className="colour-hue-row">
          <span>Hue</span>
          <input
            type="range"
            className="colour-hue"
            data-testid="colour-hue"
            min={0}
            max={359}
            step={1}
            value={hue ?? 0}
            aria-label="Accent hue"
            style={{ background: HUE_TRACK }}
            onChange={(e) => change({ accentHue: Number(e.target.value) }, true)}
          />
          <span className="colour-hue-value tabular">{hue === null ? 'Skin' : `${hue}°`}</span>
        </label>
        <div className="colour-options">
          <Segmented<Tint>
            label="Background tint"
            testId="colour-tint"
            value={colours.tint}
            options={[
              { value: 'none', label: 'No tint', disabled: hue === null },
              { value: 'soft', label: 'Soft tint', disabled: hue === null },
              { value: 'strong', label: 'Strong tint', disabled: hue === null },
            ]}
            onChange={(tint) => change({ tint })}
          />
          <Checkbox label="More contrast" data-testid="colour-contrast" checked={colours.contrast === 'more'} onChange={(on) => change({ contrast: on ? 'more' : 'standard' })} />
          <Button variant="text" data-testid="colour-reset" disabled={isDefaultColours(colours)} onClick={() => change({ ...COLOURS_DEFAULT })}>
            Reset
          </Button>
        </div>
        {adjusted && (
          <div className="colour-adjusted" data-testid="colour-adjusted" role="status">
            Adjusted to stay readable.
          </div>
        )}
      </div>
    </SettingRow>
  );
}
