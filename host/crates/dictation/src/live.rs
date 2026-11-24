//! Live dictation: words while you speak, not one answer at the end.
//!
//! On Windows the session runs on the operating system's own speech service
//! (`Windows.Media.SpeechRecognition`): a continuous recognition session with the dictation
//! scenario reports words as it hears them (the `HypothesisGenerated` event) and settles each
//! sentence when the speaker pauses (the `ResultGenerated` event). Nothing is recorded, sent to a
//! server, or stored: the words come from the speech engine on this machine.
//!
//! One session at a time owns the microphone. `start` spins a thread of its own (COM's apartment
//! never touches the async runtime's threads, the way `helm-platform`'s audio enumeration works),
//! streams the words out as events, and obeys `stop` (finish, keep what was heard) or `cancel`
//! (throw the unfinished sentence away). A session that ends by itself — the microphone taken, the
//! engine stopped — says so in one line and its words up to that point still settle into the
//! draft.
//!
//! | event | payload |
//! |---|---|
//! | `live_transcript` | `{final, text}` — `text` is the sentence in progress (replaces the last) or the settled sentence |
//! | `dictation_live` | `{state: "failed", message}` — the session ended by itself |
//!
//! Off Windows, or where the speech service has no model installed, `available` answers false and
//! `start` answers with its one-line reason; the caller keeps the batch path.

use helm_ipc::{ApiError, AppEvent, EventSink};
use std::sync::mpsc::{channel, sync_channel, Receiver, Sender, SyncSender, TryRecvError};
use std::sync::{Arc, Mutex, OnceLock};
use std::thread::JoinHandle;
use std::time::{Duration, Instant};

/// The word stream of a live session.
pub const TRANSCRIPT_EVENT: &str = "live_transcript";
/// A live session that ended by itself: `{state: "failed", message}`.
pub const STATE_EVENT: &str = "dictation_live";

/// What the engine reports while it listens.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum LiveMsg {
    /// More of the sentence in progress; the caller accumulates it.
    Words(String),
    /// The sentence in progress re-written whole; it replaces what came before.
    Replace(String),
    /// The sentence in progress is finished: its settled text.
    Final(String),
    /// The engine can no longer hear; the session is over.
    Failed(String),
    /// The engine ended the session by itself.
    Ended,
}

/// What the page asks of a running session.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Cmd {
    Stop,
    Cancel,
}

/// The engine behind one live session: start it, read its words, end it. Windows implements it
/// over the speech service; tests script it.
pub trait Engine: Send {
    /// Open the microphone and begin. An error is the one line the user should see.
    fn start(&mut self) -> Result<(), String>;
    /// The next message, or `None` when nothing came within `wait`.
    fn next(&mut self, wait: Duration) -> Option<LiveMsg>;
    /// End the session the proper way.
    fn stop(&mut self) -> Result<(), String>;
    /// End the session without finishing the sentence.
    fn cancel(&mut self);
}

pub trait EngineFactory: Send + Sync {
    fn create(&self) -> Result<Box<dyn Engine>, String>;
}

/// Whether a live session can run here, and why not when it cannot. Answered once per process:
/// probing activates the speech service, which is not free.
pub fn available() -> (bool, Option<String>) {
    static CACHE: OnceLock<(bool, Option<String>)> = OnceLock::new();
    CACHE.get_or_init(probe).clone()
}

#[cfg(windows)]
fn probe() -> (bool, Option<String>) {
    // A thread of its own: the probe activates the speech service and leaves again.
    std::thread::spawn(|| {
        let rt = unsafe {
            windows::Win32::System::WinRT::RoInitialize(
                windows::Win32::System::WinRT::RO_INIT_MULTITHREADED,
            )
        };
        if rt.is_err() {
            return (
                false,
                Some("Windows speech recognition could not start on this PC.".to_owned()),
            );
        }
        let out = match windows::Media::SpeechRecognition::SpeechRecognizer::new() {
            Ok(_recognizer) => (true, None),
            Err(e) => (false, Some(start_failure(&e))),
        };
        unsafe { windows::Win32::System::WinRT::RoUninitialize() };
        out
    })
    .join()
    .unwrap_or((
        false,
        Some("Windows speech recognition could not be checked.".to_owned()),
    ))
}

#[cfg(not(windows))]
fn probe() -> (bool, Option<String>) {
    (false, Some("Live dictation needs Windows.".to_owned()))
}

/// Turn one engine hypothesis into what the word stream should carry. The speech service
/// re-states the whole sentence in progress on every hypothesis, never just the new words, so
/// the raw text must be folded back into the stream `drive` accumulates: the new tail when the
/// engine only added words, the whole replacement when it re-wrote the sentence mid-way (it
/// does, as it hears a word it mis-heard the first time). `prev` is the last hypothesis seen
/// and stays the whole text.
pub(crate) fn hypothesis_update(prev: &mut String, text: String) -> Option<LiveMsg> {
    if text.trim().is_empty() {
        return None;
    }
    let msg = match text.strip_prefix(prev.as_str()) {
        Some(tail) if !tail.is_empty() => LiveMsg::Words(tail.to_owned()),
        Some(_) => return None, // the very same sentence again: no news
        None => LiveMsg::Replace(text.clone()),
    };
    *prev = text;
    Some(msg)
}

/// One live session: the command line to its thread and the thread itself.
struct Running {
    cmd: Sender<Cmd>,
    thread: JoinHandle<()>,
}

pub struct LiveDictation {
    slot: Mutex<Option<Running>>,
    factory: Arc<dyn EngineFactory>,
}

impl Default for LiveDictation {
    fn default() -> Self {
        Self::new()
    }
}

impl LiveDictation {
    pub fn new() -> Self {
        Self {
            slot: Mutex::new(None),
            factory: Arc::new(PlatformEngines),
        }
    }

    #[cfg(test)]
    pub fn with_factory(factory: Arc<dyn EngineFactory>) -> Self {
        Self {
            slot: Mutex::new(None),
            factory,
        }
    }

    /// Begin a session. The words arrive on `events` afterwards.
    pub fn start(&self, events: Arc<dyn EventSink>) -> Result<(), ApiError> {
        let mut slot = self.slot.lock().unwrap();
        if let Some(r) = slot.take() {
            if r.thread.is_finished() {
                let _ = r.thread.join(); // it ended by itself: reap it
            } else {
                *slot = Some(r);
                return Err(ApiError::new(
                    "busy",
                    "Dictation is already listening. Finish it, or press Esc.",
                ));
            }
        }
        let engine = self
            .factory
            .create()
            .map_err(|m| ApiError::new("not_available", m))?;
        let (cmd_tx, cmd_rx) = channel();
        let (ack_tx, ack_rx) = sync_channel(1);
        let thread = std::thread::spawn(move || drive(engine, cmd_rx, ack_tx, events));
        // The lock is held across the handshake: a `stop` that arrives while the session is
        // still opening waits its turn here and then finds the session, never an empty slot.
        // The session thread never takes this lock, so waiting here can never deadlock.
        match ack_rx.recv_timeout(Duration::from_secs(20)) {
            Ok(Ok(())) => {
                *slot = Some(Running {
                    cmd: cmd_tx,
                    thread,
                });
                Ok(())
            }
            Ok(Err(m)) => {
                let _ = thread.join();
                Err(ApiError::new("not_available", m))
            }
            _ => {
                // The thread went away or the service never answered: close what may have opened.
                let _ = cmd_tx.send(Cmd::Cancel);
                let _ = thread.join();
                Err(ApiError::new(
                    "not_available",
                    "Windows speech recognition did not answer in time.",
                ))
            }
        }
    }

    pub fn stop(&self) -> Result<(), ApiError> {
        self.finish(Cmd::Stop)
    }

    pub fn cancel(&self) -> Result<(), ApiError> {
        self.finish(Cmd::Cancel)
    }

    /// Send the word to the session and wait for its thread to end. Idempotent: a session that
    /// ended by itself answers the same way.
    fn finish(&self, cmd: Cmd) -> Result<(), ApiError> {
        let r = self.slot.lock().unwrap().take();
        let Some(r) = r else { return Ok(()) };
        let _ = r.cmd.send(cmd);
        let _ = r.thread.join();
        Ok(())
    }
}

/// The session's life: start, stream the words, then obey the end word. Any sentence still in
/// progress settles first — the user heard the words, so they keep them.
fn drive(
    mut engine: Box<dyn Engine>,
    cmd_rx: Receiver<Cmd>,
    ack_tx: SyncSender<Result<(), String>>,
    events: Arc<dyn EventSink>,
) {
    match engine.start() {
        Ok(()) => {
            let _ = ack_tx.send(Ok(()));
        }
        Err(e) => {
            let _ = ack_tx.send(Err(e));
            return;
        }
    }
    let mut pending = String::new();
    loop {
        match cmd_rx.try_recv() {
            Ok(Cmd::Stop) => {
                engine.stop().ok();
                // The engine may have settled the last sentence on its way out (it fires before
                // the stop completes); its word is better than the tail we last saw, so drain it.
                while let Some(m) = engine.next(Duration::from_millis(0)) {
                    match m {
                        LiveMsg::Words(w) => pending.push_str(&w),
                        LiveMsg::Replace(t) => {
                            pending.clear();
                            pending.push_str(&t);
                        }
                        LiveMsg::Final(t) => {
                            pending.clear();
                            pending.push_str(&t);
                        }
                        LiveMsg::Failed(_) | LiveMsg::Ended => {}
                    }
                }
                flush(&mut pending, &events);
                return;
            }
            Ok(Cmd::Cancel) => {
                engine.cancel();
                return;
            }
            Err(TryRecvError::Disconnected) => {
                engine.cancel();
                return;
            }
            Err(TryRecvError::Empty) => {}
        }
        match engine.next(Duration::from_millis(200)) {
            Some(LiveMsg::Words(w)) => {
                pending.push_str(&w);
                emit_in_progress(&pending, &events);
            }
            Some(LiveMsg::Replace(t)) => {
                pending.clear();
                pending.push_str(&t);
                emit_in_progress(&pending, &events);
            }
            Some(LiveMsg::Final(t)) => {
                pending.clear();
                let t = t.trim().to_owned();
                if !t.is_empty() {
                    events.emit(
                        AppEvent::new(TRANSCRIPT_EVENT)
                            .with("final", true)
                            .with("text", t),
                    );
                }
            }
            Some(LiveMsg::Failed(m)) => {
                flush(&mut pending, &events);
                events.emit(
                    AppEvent::new(STATE_EVENT)
                        .with("state", "failed")
                        .with("message", m),
                );
                return;
            }
            Some(LiveMsg::Ended) => {
                flush(&mut pending, &events);
                return;
            }
            None => {}
        }
    }
}

/// The sentence in progress, as one replace-the-last event.
fn emit_in_progress(pending: &str, events: &Arc<dyn EventSink>) {
    let t = pending.trim().to_owned();
    if !t.is_empty() {
        events.emit(
            AppEvent::new(TRANSCRIPT_EVENT)
                .with("final", false)
                .with("text", t),
        );
    }
}

fn flush(pending: &mut String, events: &Arc<dyn EventSink>) {
    let t = std::mem::take(pending);
    let t = t.trim().to_owned();
    if !t.is_empty() {
        events.emit(
            AppEvent::new(TRANSCRIPT_EVENT)
                .with("final", true)
                .with("text", t),
        );
    }
}

struct PlatformEngines;

#[cfg(windows)]
impl EngineFactory for PlatformEngines {
    fn create(&self) -> Result<Box<dyn Engine>, String> {
        Ok(Box::<WinRtEngine>::default())
    }
}

#[cfg(not(windows))]
impl EngineFactory for PlatformEngines {
    fn create(&self) -> Result<Box<dyn Engine>, String> {
        Err(probe()
            .1
            .unwrap_or_else(|| "Live dictation is not available here.".to_owned()))
    }
}

// ---- the Windows speech service --------------------------------------------------------------

/// The speech service is spoken to from the session thread only, on a Runtime apartment of its
/// own. The three recognition events arrive on the engine's own threads and post into the
/// channel; the session thread reads them and talks to the page.
#[cfg(windows)]
struct WinRtEngine {
    tx: Sender<LiveMsg>,
    rx: Receiver<LiveMsg>,
    inner: Option<Windows>,
}

#[cfg(windows)]
struct Windows {
    recognizer: windows::Media::SpeechRecognition::SpeechRecognizer,
    session: windows::Media::SpeechRecognition::SpeechContinuousRecognitionSession,
    result_token: i64,
    completed_token: i64,
    hypothesis_token: i64,
    started: bool,
}

#[cfg(windows)]
impl Default for WinRtEngine {
    fn default() -> Self {
        let (tx, rx) = channel();
        Self {
            tx,
            rx,
            inner: None,
        }
    }
}

#[cfg(windows)]
impl Engine for WinRtEngine {
    fn start(&mut self) -> Result<(), String> {
        use windows::core::{Ref, HSTRING};
        use windows::Foundation::TypedEventHandler;
        use windows::Media::SpeechRecognition::{
            SpeechContinuousRecognitionCompletedEventArgs,
            SpeechContinuousRecognitionResultGeneratedEventArgs,
            SpeechContinuousRecognitionSession, SpeechRecognitionHypothesisGeneratedEventArgs,
            SpeechRecognitionResultStatus, SpeechRecognitionScenario,
            SpeechRecognitionTopicConstraint, SpeechRecognizer,
        };

        // SAFETY: this thread is owned by the session; RoInitialize runs once on it and
        // RoUninitialize runs in Drop after every interface is released. The recogniser is
        // agile, so the multithreaded apartment never shares COM pointers with other threads.
        unsafe {
            windows::Win32::System::WinRT::RoInitialize(
                windows::Win32::System::WinRT::RO_INIT_MULTITHREADED,
            )
            .map_err(|e| start_failure(&e))?;
        }
        let built = (|| -> windows::core::Result<Windows> {
            {
                use windows::core::Error as WError;
                let recognizer = SpeechRecognizer::new()?;
                let hint = HSTRING::from("helm dictation");
                let constraint = SpeechRecognitionTopicConstraint::Create(
                    SpeechRecognitionScenario::Dictation,
                    &hint,
                )?;
                recognizer.Constraints()?.Append(&constraint)?;
                let compiled = wait_op(
                    &recognizer.CompileConstraintsAsync()?,
                    Duration::from_secs(15),
                )?;
                if compiled.Status()? != SpeechRecognitionResultStatus::Success {
                    return Err(WError::new(
                        windows::core::HRESULT(0x8004_5000u32 as i32),
                        "Windows speech recognition could not load its dictation model.",
                    ));
                }
                let session = recognizer.ContinuousRecognitionSession()?;

                // The hypothesis handlers see whole sentences, so they fold each one against
                // the last through this; a settled sentence starts the next one from nothing.
                let heard = Arc::new(Mutex::new(String::new()));
                let tx = self.tx.clone();
                let heard_for_result = heard.clone();
                let result_handler = TypedEventHandler::new(
                    move |_s: Ref<SpeechContinuousRecognitionSession>,
                          args: Ref<SpeechContinuousRecognitionResultGeneratedEventArgs>|
                          -> windows::core::Result<()> {
                        // A settled sentence; an empty one (nothing heard in this pause) is no news.
                        let text = args.ok()?.Result()?.Text()?;
                        let t = text.to_string();
                        if !t.trim().is_empty() {
                            let _ = tx.send(LiveMsg::Final(t));
                            *heard_for_result.lock().unwrap() = String::new();
                        }
                        Ok(())
                    },
                );
                let result_token = session.ResultGenerated(&result_handler)?;
                let tx = self.tx.clone();
                let completed_handler = TypedEventHandler::new(
                    move |_s: Ref<SpeechContinuousRecognitionSession>,
                          args: Ref<SpeechContinuousRecognitionCompletedEventArgs>|
                          -> windows::core::Result<()> {
                        let status = args.ok()?.Status()?;
                        let msg = if status == SpeechRecognitionResultStatus::Success
                            || status == SpeechRecognitionResultStatus::UserCanceled
                        {
                            LiveMsg::Ended
                        } else {
                            LiveMsg::Failed(end_message(status))
                        };
                        let _ = tx.send(msg);
                        Ok(())
                    },
                );
                let completed_token = session.Completed(&completed_handler)?;
                let tx = self.tx.clone();
                let heard_for_hyp = heard.clone();
                let hypothesis_handler = TypedEventHandler::new(
                    move |_s: Ref<SpeechRecognizer>,
                          args: Ref<SpeechRecognitionHypothesisGeneratedEventArgs>|
                          -> windows::core::Result<()> {
                        // Words as they are heard, for the sentence in progress. The service
                        // re-states the whole sentence each time, so only the new tail (or the
                        // whole re-written sentence) goes on the stream.
                        let text = args.ok()?.Hypothesis()?.Text()?;
                        let mut heard = heard_for_hyp.lock().unwrap();
                        if let Some(m) = hypothesis_update(&mut heard, text.to_string()) {
                            let _ = tx.send(m);
                        }
                        Ok(())
                    },
                );
                let hypothesis_token = recognizer.HypothesisGenerated(&hypothesis_handler)?;
                wait_action(&session.StartAsync()?, Duration::from_secs(10))?;
                Ok(Windows {
                    recognizer,
                    session,
                    result_token,
                    completed_token,
                    hypothesis_token,
                    started: true,
                })
            }
        })();
        match built {
            Ok(inner) => {
                self.inner = Some(inner);
                Ok(())
            }
            Err(e) => {
                // The apartment was opened; the thread is leaving, so balance it here.
                unsafe { windows::Win32::System::WinRT::RoUninitialize() };
                Err(start_failure(&e))
            }
        }
    }

    fn next(&mut self, wait: Duration) -> Option<LiveMsg> {
        self.rx.recv_timeout(wait).ok()
    }

    fn stop(&mut self) -> Result<(), String> {
        let Some(w) = self.inner.as_ref() else {
            return Ok(());
        };
        if !w.started {
            return Ok(());
        }
        match w.session.StopAsync() {
            Ok(action) => wait_action(&action, Duration::from_secs(5)).map_err(|e| e.to_string()),
            Err(e) => Err(e.to_string()),
        }
    }

    fn cancel(&mut self) {
        let Some(w) = self.inner.as_ref() else { return };
        if !w.started {
            return;
        }
        w.session.CancelAsync().ok();
    }
}

/// Releases the session's engine objects (and the apartment) when the session thread is done.
/// The engine's event registrations die with the recogniser: `Close` is its own teardown.
#[cfg(windows)]
impl Drop for Windows {
    fn drop(&mut self) {
        let _ = self.session.RemoveResultGenerated(self.result_token);
        let _ = self.session.RemoveCompleted(self.completed_token);
        let _ = self
            .recognizer
            .RemoveHypothesisGenerated(self.hypothesis_token);
        if self.started {
            // Do not wait for a session still running: the thread is leaving it.
            self.session.CancelAsync().ok();
        }
        self.recognizer.Close().ok();
        unsafe { windows::Win32::System::WinRT::RoUninitialize() };
    }
}

#[cfg(windows)]
fn wait_action(
    action: &windows_future::IAsyncAction,
    timeout: Duration,
) -> windows::core::Result<()> {
    use windows_future::AsyncStatus;
    let began = Instant::now();
    loop {
        match action.Status()? {
            s if s == AsyncStatus::Completed => return action.GetResults(),
            s if s == AsyncStatus::Canceled => {
                return Err(windows::core::Error::new(
                    windows::core::HRESULT(0x8007_04c7u32 as i32),
                    "canceled",
                ))
            }
            s if s == AsyncStatus::Error => return action.GetResults(),
            _ => {}
        }
        if began.elapsed() > timeout {
            return Err(windows::core::Error::new(
                windows::core::HRESULT(0x8000_4005u32 as i32),
                "Windows speech recognition took too long to answer.",
            ));
        }
        std::thread::sleep(Duration::from_millis(25));
    }
}

#[cfg(windows)]
fn wait_op<T: windows::core::RuntimeType + Clone>(
    op: &windows_future::IAsyncOperation<T>,
    timeout: Duration,
) -> windows::core::Result<T> {
    use windows_future::AsyncStatus;
    let began = Instant::now();
    loop {
        match op.Status()? {
            s if s == AsyncStatus::Completed => return op.GetResults(),
            s if s == AsyncStatus::Canceled => {
                return Err(windows::core::Error::new(
                    windows::core::HRESULT(0x8007_04c7u32 as i32),
                    "canceled",
                ))
            }
            s if s == AsyncStatus::Error => return op.GetResults(),
            _ => {}
        }
        if began.elapsed() > timeout {
            return Err(windows::core::Error::new(
                windows::core::HRESULT(0x8000_4005u32 as i32),
                "Windows speech recognition took too long to answer.",
            ));
        }
        std::thread::sleep(Duration::from_millis(25));
    }
}

/// What the user is told when the service cannot open at all.
#[cfg(windows)]
pub(crate) fn start_failure(e: &windows::core::Error) -> String {
    // The engine's own HRESULTs say "no speech model for this language" far too loudly; the
    // line the user needs says what to do.
    match e.code().0 as u32 {
        0x8004_0154 | 0x8004_01e4 => {
            "Windows speech recognition is not part of this Windows. Dictation keeps the ordinary recording path.".to_owned()
        }
        // The service refuses to listen until its privacy consent is accepted on the PC; on
        // Windows that is the Speech page in Settings, the same switch dictation obeys.
        0x8004_5509 => {
            "Windows speech recognition hasn't been allowed to listen yet. Turn it on in Windows Settings, under Privacy & security → Speech.".to_owned()
        }
        0x8004_5000..=0x8004_5fff => {
            "Windows speech recognition has no model for your Windows language. Add one in Windows Settings, under Time & language.".to_owned()
        }
        _ => format!("Windows speech recognition unavailable: {e}"),
    }
}

/// Why a session that ended by itself ended. The status enum grows between Windows versions, so
/// the known words come first and the number catches the rest.
#[cfg(windows)]
fn end_message(status: windows::Media::SpeechRecognition::SpeechRecognitionResultStatus) -> String {
    use windows::Media::SpeechRecognition::SpeechRecognitionResultStatus as S;
    if status == S::MicrophoneUnavailable {
        "Windows speech recognition can't reach the microphone: another app has it, or Windows has speech turned off.".to_owned()
    } else if status.0 == 12 {
        // PrivacyStopped, not in this build's metadata.
        "Windows speech recognition was turned off in your privacy settings.".to_owned()
    } else if status == S::TimeoutExceeded {
        "Windows speech recognition timed out.".to_owned()
    } else {
        format!("Windows speech recognition stopped (code {}).", status.0)
    }
}
