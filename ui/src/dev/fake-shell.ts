// Dev fake host: the shell's and the terminal's ops. Two microphones, settings kept
// in memory, and an echoing fake terminal.

import type { FakeOpHandler } from './fakeHost.ts';

export function shellFakeOps(): { prefixes: string[]; handler: FakeOpHandler } {
  let settings = {
    globalHotkey: 'Ctrl+Alt+H' as string | null,
    keepAwake: true,
    notifications: 'in_app',
    dictationMic: null as { uid: string; name: string } | null,
    terminalShell: 'pwsh',
  };
  const mics = [
    { id: 'mic-array', name: 'Microphone Array (Realtek)', isDefault: true, available: true },
    { id: 'mic-usb', name: 'USB Microphone', isDefault: false, available: true },
  ];
  // A fake terminal: echoes input back, and answers `echo X` with X.
  const terms = new Map<string, { seq: number; line: string; cwd: string }>();
  let nextId = 1;
  const out = (ctx: Parameters<FakeOpHandler>[2], id: string, data: string) => {
    const t = terms.get(id);
    if (!t) return;
    t.seq += 1;
    ctx.emit({ kind: 'terminal', id, seq: t.seq, data });
  };
  const handler: FakeOpHandler = (op, args, ctx) => {
    switch (op) {
      case 'terminal_open': {
        const id = `term-${nextId++}`;
        const cwd = String(args.cwd ?? 'C:\\Users\\demo');
        terms.set(id, { seq: 0, line: '', cwd });
        setTimeout(() => out(ctx, id, `PS ${cwd}> `), 10);
        return { id, shell: 'pwsh', cwd, title: 'pwsh', running: true };
      }
      case 'terminal_attach': {
        const t = terms.get(String(args.id));
        if (!t) throw new Error('That terminal is closed.');
        return { session: { id: args.id, shell: 'pwsh', cwd: t.cwd, title: 'pwsh', running: true }, replay: '', seq: t.seq };
      }
      case 'terminal_write': {
        const id = String(args.id);
        const t = terms.get(id);
        if (!t) throw new Error('That terminal is closed.');
        for (const ch of String(args.data ?? '')) {
          if (ch === '\r') {
            const m = /^echo (.*)$/.exec(t.line.trim());
            out(ctx, id, `\r\n${m ? `${m[1]}\r\n` : ''}PS ${t.cwd}> `);
            t.line = '';
          } else {
            t.line += ch;
            out(ctx, id, ch);
          }
        }
        return null;
      }
      case 'terminal_ack':
      case 'terminal_resize':
        return null;
      case 'terminal_close':
        terms.delete(String(args.id));
        return null;
      case 'terminal_sessions':
        return [...terms.entries()].map(([id, t]) => ({ id, shell: 'pwsh', cwd: t.cwd, title: 'pwsh', running: true }));
      case 'shell_settings_get':
        return { settings, hotkeyRegistered: settings.globalHotkey !== null, hotkeyError: null, keepAwakeError: null };
      case 'shell_settings_save':
        settings = { ...settings, ...(args as typeof settings) };
        return { settings, hotkeyRegistered: settings.globalHotkey !== null, hotkeyError: null, keepAwakeError: null };
      case 'audio_inputs':
        return mics;
      case 'open_external':
        return null;
      default:
        throw new Error(`fake shell: unknown op ${op}`);
    }
  };
  return { prefixes: ['shell_settings_', 'audio_inputs', 'open_external', 'terminal_'], handler };
}
