// The global hotkey's checks against the one key map: a chord, not reserved,
// and not already used by an app command. The host checks again when it registers.

import { normaliseChord, RESERVED } from '../../ui/keys.ts';
import { GLOBAL_HOTKEY_BINDING } from '../../api/shell.ts';

export function hotkeyProblem(text: string, bindings: { id: string; title: string; chord: string | null }[]): string | null {
  const n = normaliseChord(text);
  if (!n) return `"${text}" is not a shortcut.`;
  const mods = n.split('+').slice(0, -1);
  if (mods.length === 0 && !/^F([1-9]|1\d|2[0-4])$/.test(n)) return 'A global hotkey needs Ctrl, Alt or Shift.';
  if ((RESERVED as readonly string[]).includes(n) || mods.includes('Super')) return `${n} belongs to Windows and cannot be used.`;
  const clash = bindings.find((b) => b.id !== GLOBAL_HOTKEY_BINDING && b.chord === n);
  if (clash) return `${n} is already used by "${clash.title}".`;
  return null;
}
