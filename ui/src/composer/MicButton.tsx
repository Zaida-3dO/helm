// The composer's microphone: a button that becomes a "Listening" pill while it listens, then a
// "Writing it down…" pill while the host turns the recording into words. A tap on the button or
// on Ctrl+Shift+Space starts listening and a second tap finishes; holding either talks until
// release. The words land at the caret and are never sent. Esc cancels (no request is made),
// Enter or a click finishes. The rules are in dictation.ts; this file is the screen part.
//
// Where the Windows speech service can run and no speech-to-text server is set up, the mic is
// live: the host streams words while the user speaks, settled sentences land in the draft as they
// come and the sentence in progress is handed to `onLiveText` for the editor to show. A configured
// server wins: the user's own server and language beat the operating system's speech service.
// There is no recording, no level bars (the engine holds the microphone) and no second wait.
//
// Both composers mount a mic button, but a live session has one owner: only the button that
// started it applies its words. Pressing the mic in the other composer takes the session over —
// the host takes the microphone from the old session — and the first button falls back to idle.

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
  LIVE_IDLE,
  liveStep,
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
  type LiveState,
  type Problem,
} from './dictation.ts';
import './dictation.css';

type Status = 'idle' | 'listening' | 'transcribing' | 'error';

/** A recording shorter than this has nothing in it worth sending. */
const MIN_SECONDS = 0.3;

/**
 * The claim of the button that started the one live session. A session's words only land in its
 * owner's composer, even though every open composer mounts a mic button; the claim clears once
 * the host's stop or cancel has answered, so the last sentence settled during a stop still lands.
 */
let liveOwner: symbol | null = null;
/** Buttons watch the owner: a takeover sends the button that lost its session back to idle. */
const liveOwnerListeners = new Set<() => void>();
function setLiveOwner(claim: symbol | null): void {
  liveOwner = claim;
  for (const l of liveOwnerListeners) l();
}

/** Whether decoration may animate, and a re-render when that changes. */
function useMayMove(): boolean {
  const [may, setMay] = useState(mayMove());
  useEffect(() => onMotionChange(() => setMay(mayMove())), []);
  return may;
}

export function MicButton({
  editor,
  onLiveText,
}: {
  editor: RefObject<EditorHandle | null>;
  /** The sentence in progress of a live session, for the editor to show; '' when there is none. */
  onLiveText: (text: string) => void;
}) {
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
  /** A live session (the host's speech service, no Recorder) is the one this button owns. */
  const liveMode = useRef(false);
  /** This button's claim on the one live session; null while it holds none. */
  const liveClaim = useRef<symbol | null>(null);
  /** The sentence in progress of a live session; settled sentences go straight to the draft. */
  const liveState = useRef<LiveState>(LIVE_IDLE);
  const liveStart = useRef(0);

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

  // A live session's words. A settled sentence lands at the caret the moment it arrives; the
  // sentence in progress is handed to the editor, which shows it after the draft. Only the
  // session's owner applies them: every open composer hears the same events.
  useEffect(() => {
    const owns = () => liveClaim.current !== null && liveOwner === liveClaim.current;
    const offWords = shell.bus.on('live_transcript', (e) => {
      if (!owns()) return;
      const [next, settled] = liveStep(liveState.current, {
        final: e.final === true,
        text: typeof e.text === 'string' ? e.text : '',
      });
      liveState.current = next;
      onLiveText(next.provisional);
      if (settled) place(settled);
    });
    const offState = shell.bus.on('dictation_live', (e) => {
      if (e.state !== 'failed' || !owns()) return;
      liveMode.current = false;
      liveState.current = LIVE_IDLE;
      onLiveText('');
      showProblem({
        message: typeof e.message === 'string' ? e.message : 'Live dictation stopped.',
        retry: false,
        settings: false,
      });
    });
    return () => {
      offWords();
      offState();
    };
  }, [shell, place, onLiveText, showProblem]);

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

  /** Let go of this button's claim on the live session, if nothing has taken it already. */
  const giveUpLive = useCallback((claim: symbol | null) => {
    if (claim && liveClaim.current === claim) {
      liveClaim.current = null;
      setLiveOwner(null);
    }
  }, []);

  const finish = useCallback(
    async (capped = false) => {
      if (liveMode.current) {
        // The host settles the sentence in progress and sends it as a final word. The claim is
        // only given up once the host has answered, so that last word still lands here.
        const claim = liveClaim.current;
        liveMode.current = false;
        keyState.current = IDLE;
        generation.current++;
        setStatus('idle');
        ready.current = ops
          .live('stop')
          .then(
            () => undefined,
            (e: unknown) => {
              if (alive.current) showProblem(problemOf(e));
            },
          )
          .finally(() => giveUpLive(claim));
        return;
      }
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
    [ops, send, showProblem, setStatus, giveUpLive],
  );

  /** Throw the recording away (Esc), or ignore a request still in flight. No request is made. */
  const cancel = useCallback(() => {
    generation.current++;
    keyState.current = IDLE;
    if (liveMode.current) {
      // The host throws the unfinished sentence away; nothing is placed. The claim is kept
      // until the host answers, so a sentence that had already settled still lands.
      const claim = liveClaim.current;
      liveMode.current = false;
      liveState.current = LIVE_IDLE;
      onLiveText('');
      void ops
        .live('cancel')
        .then(
          () => giveUpLive(claim),
          () => giveUpLive(claim),
        );
      clearProblem();
      setStatus('idle');
      return;
    }
    recorder.current?.cancel();
    recorder.current = null;
    lastWav.current = null;
    clearProblem();
    setStatus('idle');
  }, [clearProblem, onLiveText, ops, setStatus, giveUpLive]);

  /** Begin a live session on the host's speech service. The words arrive as events. */
  const startLive = useCallback(() => {
    const mine = ++generation.current;
    liveMode.current = true;
    liveState.current = LIVE_IDLE;
    liveStart.current = Date.now();
    setElapsed(0);
    setStatus('listening');
    ready.current = ops.live('start').then(
      () => {
        // The session is ours from the moment it opened: the host has just taken the microphone
        // from whatever session ran before (in the other composer that falls back to idle), and
        // from here the host's words are this composer's.
        if (alive.current && mine === generation.current && liveMode.current) {
          liveClaim.current = Symbol('live');
          setLiveOwner(liveClaim.current);
        }
      },
      (e: unknown) => {
        // The session never opened: let the pill go back to the microphone.
        if (alive.current && mine === generation.current && liveMode.current) {
          liveMode.current = false;
          keyState.current = IDLE;
          showProblem(problemOf(e));
        }
      },
    );
  }, [ops, showProblem, setStatus]);

  const start = useCallback(async () => {
    if (statusRef.current === 'transcribing') {
      keyState.current = IDLE; // one thing at a time
      return;
    }
    clearProblem();
    // A configured speech-to-text server wins: the user's server and their language choice beat
    // the operating system's speech service. Live dictation fills in when nothing is set up.
    if (viewRef.current?.liveAvailable && viewRef.current?.configured !== true) {
      startLive();
      await ready.current;
      return;
    }
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
  }, [clearProblem, finish, showProblem, startLive, setStatus]);

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

  // The clock and the bars. A live session has no bars: the speech service holds the microphone.
  useEffect(() => {
    if (status !== 'listening') return;
    const t = setInterval(() => {
      const r = recorder.current;
      if (r) {
        setElapsed(r.elapsedMs());
        if (may) setLevels(r.levels());
      } else if (liveMode.current) {
        setElapsed(Date.now() - liveStart.current);
      }
    }, 90);
    return () => clearInterval(t);
  }, [status, may]);

  // Another composer took the microphone: this button lost its session, so its pill goes back
  // to the microphone and the sentence in progress is let go.
  useEffect(() => {
    const check = () => {
      if (liveMode.current && liveOwner !== liveClaim.current) {
        liveMode.current = false;
        liveState.current = LIVE_IDLE;
        liveClaim.current = null;
        onLiveText('');
        setStatus('idle');
      }
    };
    liveOwnerListeners.add(check);
    return () => {
      liveOwnerListeners.delete(check);
    };
  }, [onLiveText, setStatus]);

  useEffect(() => {
    alive.current = true; // again after a strict-mode remount
    return () => {
      alive.current = false;
      clearTimeout(errorTimer.current);
      if (liveMode.current) void ops.live('cancel').catch(() => undefined);
      liveMode.current = false;
      giveUpLive(liveClaim.current);
      onLiveText('');
      recorder.current?.cancel();
      recorder.current = null;
    };
  }, [ops, onLiveText, giveUpLive]);

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
        data-live={liveMode.current ? 'true' : undefined}
        aria-label="Listening. Finish dictation"
        title="Esc to cancel · Enter or click to finish"
        onMouseDown={keepFocus}
        onPointerDown={press}
        onClick={keyboardTap}
      >
        <span className="dictation-dot" aria-hidden="true" />
        <span className="dictation-time">Listening {clock(elapsed)}</span>
        {liveMode.current ? null : (
          <span className="dictation-bars" aria-hidden="true" data-still={may ? undefined : 'true'}>
            {levels.map((v, i) => (
              <span key={i} style={{ height: `${may ? 3 + Math.round(v * 11) : 8}px` }} />
            ))}
          </span>
        )}
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
  // Usable live (the Windows speech service) or batch (a configured speech-to-text server).
  const usable = view?.configured === true || view?.liveAvailable === true;
  return (
    <Button
      variant="icon"
      className={`composer-mic${usable ? '' : ' unconfigured'}`}
      label={usable ? 'Dictate' : 'Set up dictation'}
      title={usable ? hintLabel('Dictate: tap to start and stop, or hold to talk', chord) : 'Set up dictation in Settings → Shell'}
      data-testid="composer-mic"
      onMouseDown={keepFocus}
      onPointerDown={usable ? press : undefined}
      onClick={usable ? keyboardTap : openSettings}
    >
      <IconMic size={16} />
    </Button>
  );
}
