// The composer's microphone: a button that becomes a "Listening" pill while it records, then a
// "Writing it down…" pill while the host turns the recording into words. A tap on the button or
// on Ctrl+Shift+Space starts listening and a second tap finishes; holding either talks until
// release. The words land at the caret and are never sent. Esc cancels (no request is made),
// Enter or a click finishes. The rules are in dictation.ts; this file is the screen part.

import { useCallback, useEffect, useMemo, useRef, useState, type RefObject } from 'react';
import { shellOps } from '../api/shell.ts';
import { requestPane } from '../settings/registry.ts';
import { SETTINGS_DIALOG } from '../settings/SettingsDialog.tsx';
import { mayMove, onMotionChange } from '../shell/motion.ts';
import { useDialog, useShell } from '../shell/context.ts';
import { Button } from '../ui/controls.tsx';
import { useBinding, useChord, useLayer } from '../ui/hooks.ts';
import { IconMic } from '../ui/icons.tsx';
import { hintLabel } from '../ui/keys.ts';
import { WheelMark } from '../ui/status/WheelMark.tsx';
import { toast } from '../ui/toasts.ts';
import type { EditorHandle } from './Editor.tsx';
import {
  clock,
  DICTATE_BINDING,
  DICTATE_KEYS,
  dictationOps,
  ERROR_MS,
  IDLE,
  keyStep,
  LEVEL_BARS,
  NOT_SET_UP,
  NOTHING_HEARD,
  problemOf,
  Recorder,
  releasesChord,
  spokenInsert,
  wavSeconds,
  type DictationView,
  type KeyInput,
  type KeyState,
  type Problem,
} from './dictation.ts';
import './dictation.css';

type Status = 'idle' | 'listening' | 'transcribing' | 'error';

/** A recording shorter than this has nothing in it worth sending. */
const MIN_SECONDS = 0.3;

/** Whether decoration may animate, and a re-render when that changes. */
function useMayMove(): boolean {
  const [may, setMay] = useState(mayMove());
  useEffect(() => onMotionChange(() => setMay(mayMove())), []);
  return may;
}

export function MicButton({ editor }: { editor: RefObject<EditorHandle | null> }) {
  const shell = useShell();
  const ops = useMemo(() => dictationOps(shell.api), [shell.api]);
  const settingsDialog = useDialog(SETTINGS_DIALOG);
  const may = useMayMove();
  const chord = useChord(DICTATE_BINDING);

  const [view, setView] = useState<DictationView | null>(null);
  const viewRef = useRef<DictationView | null>(null);
  viewRef.current = view;
  /** The microphone chosen in Settings → Shell; null is the Windows default. */
  const micName = useRef<string | null>(null);

  const [status, setStatusState] = useState<Status>('idle');
  const statusRef = useRef<Status>('idle');
  const setStatus = useCallback((s: Status) => {
    statusRef.current = s;
    setStatusState(s);
  }, []);
  const [problem, setProblem] = useState<Problem | null>(null);
  const [elapsed, setElapsed] = useState(0);
  const [levels, setLevels] = useState<number[]>(() => Array<number>(LEVEL_BARS).fill(0));

  const recorder = useRef<Recorder | null>(null);
  const ready = useRef<Promise<void>>(Promise.resolve());
  const keyState = useRef<KeyState>(IDLE);
  /** The last recording, held until the next one so a failed request can be sent again. */
  const lastWav = useRef<Uint8Array | null>(null);
  /** Bumped by every cancel and every start: an answer for an older one is dropped. */
  const generation = useRef(0);
  const errorTimer = useRef<ReturnType<typeof setTimeout> | undefined>(undefined);
  const alive = useRef(true);

  const load = useCallback(() => {
    ops
      .get()
      .then(setView)
      .catch(() => setView(null));
    shellOps(shell.api)
      .get()
      .then((v) => {
        micName.current = v.settings.dictationMic?.name ?? null;
      })
      .catch(() => undefined);
  }, [ops, shell.api]);
  useEffect(() => shell.bus.on('dictation', load), [shell, load]);
  // On mount, and again when Settings closes (the address or the microphone may have changed).
  useEffect(() => {
    if (!settingsDialog.open) load();
  }, [settingsDialog.open, load]);

  const clearProblem = useCallback(() => {
    clearTimeout(errorTimer.current);
    setProblem(null);
    if (statusRef.current === 'error') setStatus('idle');
  }, [setStatus]);
  const showProblem = useCallback((p: Problem) => {
    clearTimeout(errorTimer.current);
    setProblem(p);
    setStatus('error');
    errorTimer.current = setTimeout(() => {
      setProblem(null);
      if (statusRef.current === 'error') setStatus('idle');
    }, ERROR_MS);
  }, [setStatus]);

  const openSettings = useCallback(() => {
    requestPane('shell');
    shell.openDialog(SETTINGS_DIALOG);
  }, [shell]);

  /** The words go to the caret; the draft is never sent. */
  const place = useCallback((text: string) => {
    const ed = editor.current;
    if (!ed) {
      toast.show({ kind: 'info', text: 'Dictation finished after the message box closed, so the words were dropped.' });
      return;
    }
    const piece = spokenInsert(ed.getText(), ed.caretOffset(), text);
    if (piece) ed.insert(piece);
    ed.focus();
  }, [editor]);

  const send = useCallback(
    async (wav: Uint8Array) => {
      if (wavSeconds(wav) < MIN_SECONDS) {
        lastWav.current = null;
        showProblem({ message: NOTHING_HEARD, retry: false, settings: false });
        return;
      }
      const mine = ++generation.current;
      setStatus('transcribing');
      try {
        const { text } = await ops.transcribe(wav);
        if (!alive.current || mine !== generation.current) return;
        lastWav.current = null;
        if (!text.trim()) return showProblem({ message: NOTHING_HEARD, retry: false, settings: false });
        setStatus('idle');
        place(text);
      } catch (e) {
        if (!alive.current || mine !== generation.current) return;
        const p = problemOf(e);
        // Only a failure to get an answer keeps the recording for Retry.
        if (!p.retry) lastWav.current = null;
        showProblem(p);
      }
    },
    [ops, place, showProblem, setStatus],
  );

  const finish = useCallback(
    async (capped = false) => {
      const r = recorder.current;
      if (!r) return;
      await ready.current;
      if (recorder.current !== r) return; // cancelled, or the microphone never opened
      recorder.current = null;
      keyState.current = IDLE;
      const wav = r.finish();
      lastWav.current = wav;
      if (capped) toast.show({ kind: 'info', text: 'Dictation stopped at five minutes.' });
      await send(wav);
    },
    [send],
  );

  /** Throw the recording away (Esc), or ignore a request still in flight. No request is made. */
  const cancel = useCallback(() => {
    generation.current++;
    keyState.current = IDLE;
    recorder.current?.cancel();
    recorder.current = null;
    lastWav.current = null;
    clearProblem();
    setStatus('idle');
  }, [clearProblem, setStatus]);

  const start = useCallback(async () => {
    if (statusRef.current === 'transcribing') {
      keyState.current = IDLE; // one thing at a time
      return;
    }
    clearProblem();
    if (!viewRef.current?.configured) {
      keyState.current = IDLE;
      showProblem({ message: NOT_SET_UP, retry: false, settings: true });
      return;
    }
    const mine = ++generation.current;
    const r = new Recorder();
    recorder.current = r;
    lastWav.current = null;
    setElapsed(0);
    setStatus('listening');
    ready.current = r
      .start(micName.current, () => void finish(true))
      .then(() => {
        // Cancelled while the microphone was opening: let it go again.
        if (recorder.current !== r || mine !== generation.current) r.cancel();
      })
      .catch((e: unknown) => {
        if (recorder.current === r) {
          recorder.current = null;
          keyState.current = IDLE;
          showProblem(problemOf(e));
        }
      });
    await ready.current;
  }, [clearProblem, finish, showProblem, setStatus]);

  /** Feed one press, release or stop through the rule and do what it says. */
  const act = useCallback(
    (input: KeyInput) => {
      const [next, action] = keyStep(keyState.current, input, performance.now());
      keyState.current = next;
      if (action === 'start') void start();
      else if (action === 'finish') void finish();
    },
    [start, finish],
  );

  const retry = () => {
    const wav = lastWav.current;
    clearProblem();
    if (wav) void send(wav);
  };

  useBinding({ id: DICTATE_BINDING, title: 'Dictate into the message', defaultKeys: DICTATE_KEYS, run: () => act('press') });

  // The end of a press: the key coming up, or the mouse button coming up anywhere.
  useEffect(() => {
    const up = (e: KeyboardEvent) => {
      if (releasesChord(chord, e)) act('release');
    };
    const release = () => act('release');
    window.addEventListener('keyup', up, true);
    window.addEventListener('pointerup', release, true);
    window.addEventListener('blur', release);
    return () => {
      window.removeEventListener('keyup', up, true);
      window.removeEventListener('pointerup', release, true);
      window.removeEventListener('blur', release);
    };
  }, [chord, act]);

  // Enter finishes, taken before the box can turn it into a newline or a send.
  useEffect(() => {
    if (status !== 'listening') return;
    const onKey = (e: KeyboardEvent) => {
      if (e.key !== 'Enter' || e.shiftKey || e.ctrlKey || e.altKey || e.metaKey || e.isComposing) return;
      e.preventDefault();
      e.stopPropagation();
      act('stop');
      void finish();
    };
    window.addEventListener('keydown', onKey, true);
    return () => window.removeEventListener('keydown', onKey, true);
  }, [status, act, finish]);

  // Esc cancels, ahead of the composer's own Esc.
  useLayer('popover', status === 'listening' || status === 'transcribing', () => {
    cancel();
    return true;
  });

  // The clock and the bars.
  useEffect(() => {
    if (status !== 'listening') return;
    const t = setInterval(() => {
      const r = recorder.current;
      if (!r) return;
      setElapsed(r.elapsedMs());
      if (may) setLevels(r.levels());
    }, 90);
    return () => clearInterval(t);
  }, [status, may]);

  useEffect(() => {
    alive.current = true; // again after a strict-mode remount
    return () => {
      alive.current = false;
      clearTimeout(errorTimer.current);
      recorder.current?.cancel();
      recorder.current = null;
    };
  }, []);

  const keepFocus = (e: { preventDefault(): void }) => e.preventDefault();
  const press = (e: { button: number }) => {
    if (e.button === 0) act('press');
  };
  // A click with no pointer (Enter or Space on the focused button) is a whole tap.
  const keyboardTap = (e: { detail: number }) => {
    if (e.detail !== 0) return;
    act('press');
    act('release');
  };

  if (status === 'listening') {
    return (
      <button
        type="button"
        className="dictation-pill listening"
        data-testid="dictation-pill"
        data-state="listening"
        aria-label="Listening. Finish dictation"
        title="Esc to cancel · Enter or click to finish"
        onMouseDown={keepFocus}
        onPointerDown={press}
        onClick={keyboardTap}
      >
        <span className="dictation-dot" aria-hidden="true" />
        <span className="dictation-time">Listening {clock(elapsed)}</span>
        <span className="dictation-bars" aria-hidden="true" data-still={may ? undefined : 'true'}>
          {levels.map((v, i) => (
            <span key={i} style={{ height: `${may ? 3 + Math.round(v * 11) : 8}px` }} />
          ))}
        </span>
        <span className="dictation-hint">Esc to cancel · Enter or click to finish</span>
      </button>
    );
  }
  if (status === 'transcribing') {
    return (
      <span className="dictation-pill" data-testid="dictation-pill" data-state="transcribing" role="status">
        <WheelMark size={14} state="working" />
        <span>Writing it down…</span>
      </span>
    );
  }
  if (status === 'error' && problem) {
    return (
      <span className="dictation-pill error" data-testid="dictation-pill" data-state="error" role="alert">
        <span className="dictation-message" data-testid="dictation-error">
          {problem.message}
        </span>
        {problem.retry && (
          <button type="button" className="dictation-link" data-testid="dictation-retry" onClick={retry}>
            Retry
          </button>
        )}
        {problem.settings && (
          <button type="button" className="dictation-link" data-testid="dictation-settings" onClick={openSettings}>
            Open Settings
          </button>
        )}
      </span>
    );
  }
  const configured = view?.configured === true;
  return (
    <Button
      variant="icon"
      className={`composer-mic${configured ? '' : ' unconfigured'}`}
      label={configured ? 'Dictate' : 'Set up dictation'}
      title={configured ? hintLabel('Dictate: tap to start and stop, or hold to talk', chord) : 'Set up dictation in Settings → Shell'}
      data-testid="composer-mic"
      onMouseDown={keepFocus}
      onPointerDown={configured ? press : undefined}
      onClick={configured ? keyboardTap : openSettings}
    >
      <IconMic size={16} />
    </Button>
  );
}
