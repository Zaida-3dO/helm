//! Live dictation: words while you speak, not one answer at the end.
//!
//! On Windows the session runs on the operating system's own speech service
//! (`Windows.Media.SpeechRecognition`): a continuous recognition session with the dictation
//! scenario reports words as it hears them (the `HypothesisGenerated` event) and settles each
//! sentence when the speaker pauses (the `ResultGenerated` event). Nothing is recorded, sent to a
//! server, or stored: the words come from the speech engine on this machine.
//!
//! One session at a time owns the microphone, and `start` takes it from whatever session was
//! running before: a page reload never asks the old session to close first, and only one owner
//! can hold the microphone at a time. `start` spins a thread of its own (COM's apartment never
//! touches the async runtime's threads, the way `helm-platform`'s audio enumeration works),
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
use std::sync::{Arc, Mutex};
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
    /// Make an engine for one session. `language` is the BCP-47 tag the user chose, or `None`
    /// to leave the choice to the operating system.
    fn create(&self, language: Option<&str>) -> Result<Box<dyn Engine>, String>;
}

/// How long a "cannot run here" answer stands before the service is asked again (a language
/// pack or the privacy switch may have been turned on since).
const RETRY_AFTER: Duration = Duration::from_secs(30);

/// Whether a live session can run here, and why not when it cannot. A yes is kept for the life
/// of the process: probing activates the speech service, which is not free. A no is asked again
/// after [`RETRY_AFTER`], so fixing the cause in Windows Settings does not need a restart.
/// Whether it can run, and why not when it cannot; stamped with the moment of the probe.
type Availability = (bool, Option<String>);

pub fn available() -> Availability {
    static CACHE: Mutex<Option<(Instant, Availability)>> = Mutex::new(None);
    let mut cache = CACHE.lock().unwrap_or_else(|p| p.into_inner());
    if let Some((at, answer)) = cache.as_ref() {
        if answer.0 || at.elapsed() < RETRY_AFTER {
            return answer.clone();
        }
    }
    let answer = probe();
    *cache = Some((Instant::now(), answer.clone()));
    answer
}

#[cfg(windows)]
fn probe() -> (bool, Option<String>) {
    // A thread of its own: the probe activates the speech service and leaves again. It builds a
    // recognizer the way a session does (the same languages, in the same order), so a yes here
    // means a session can open one.
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
        // Balances the RoInitialize above after every recognizer below is gone.
        let _apartment = UninitializeOnDrop;
        let mut first_failure: Option<String> = None;
        for tag in candidate_languages(None) {
            match create_recognizer(tag.as_deref()) {
                Ok(recognizer) => {
                    recognizer.Close().ok();
                    return (true, None);
                }
                Err(e) => {
                    log::warn!(
                        "live dictation probe: {} failed: {e}",
                        tag.as_deref().unwrap_or("the default language")
                    );
                    first_failure.get_or_insert_with(|| start_failure(&e));
                }
            }
        }
        (
            false,
            Some(first_failure.unwrap_or_else(|| {
                "Windows speech recognition could not start on this PC.".to_owned()
            })),
        )
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

    /// Begin a session, taking the microphone from any session still running (there is only
    /// ever one owner, and a page reload never told the old session to close). `language` is
    /// the BCP-47 tag the user chose, or `None` for the operating system's own speech language.
    /// The words arrive on `events` afterwards.
    pub fn start(
        &self,
        events: Arc<dyn EventSink>,
        language: Option<String>,
    ) -> Result<(), ApiError> {
        let mut slot = self.slot.lock().unwrap();
        if let Some(r) = slot.take() {
            if r.thread.is_finished() {
                let _ = r.thread.join(); // it ended by itself: reap it
            } else {
                // Take over: the unfinished sentence goes the way Esc would have thrown it.
                let _ = r.cmd.send(Cmd::Cancel);
                let _ = r.thread.join();
            }
        }
        let engine = self
            .factory
            .create(language.as_deref())
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
                // The engine may have settled sentences on its way out (they fire before the
                // stop completes); each settled one is sent as it is drained, so two of them
                // are never collapsed into one, and only the sentence still in progress is kept.
                while let Some(m) = engine.next(Duration::from_millis(0)) {
                    match m {
                        LiveMsg::Words(w) => pending.push_str(&w),
                        LiveMsg::Replace(t) => {
                            pending.clear();
                            pending.push_str(&t);
                        }
                        LiveMsg::Final(t) => {
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
                // A settled sentence. An empty one says the engine heard nothing during the
                // pause: it still goes out, so the in-progress text it replaces is cleared
                // rather than left to settle into the draft as real words at stop.
                pending.clear();
                events.emit(
                    AppEvent::new(TRANSCRIPT_EVENT)
                        .with("final", true)
                        .with("text", t.trim().to_owned()),
                );
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
    fn create(&self, language: Option<&str>) -> Result<Box<dyn Engine>, String> {
        Ok(Box::new(WinRtEngine::new(language.map(str::to_owned))))
    }
}

#[cfg(not(windows))]
impl EngineFactory for PlatformEngines {
    fn create(&self, _language: Option<&str>) -> Result<Box<dyn Engine>, String> {
        Err(probe()
            .1
            .unwrap_or_else(|| "Live dictation is not available here.".to_owned()))
    }
}

/// Map the language the user chose onto one the speech service really has. Windows refuses a
/// tag it has no model for, and the short codes the setting offers (like `en`) are rarely tags
/// of their own: an exact tag wins; otherwise the system's own speech language when it shares
/// the chosen language's prefix; otherwise the first supported tag with that prefix; otherwise
/// `None`, which leaves the choice to Windows. `system` and the supported tags are full tags
/// like `en-GB`; matching on tags ignores case.
pub(crate) fn pick_speech_tag(chosen: &str, system: &str, supported: &[String]) -> Option<String> {
    if let Some(exact) = supported.iter().find(|t| t.eq_ignore_ascii_case(chosen)) {
        return Some(exact.clone());
    }
    let prefix = chosen.split('-').next().unwrap_or(chosen);
    if system
        .split('-')
        .next()
        .unwrap_or(system)
        .eq_ignore_ascii_case(prefix)
    {
        return Some(system.to_owned());
    }
    supported
        .iter()
        .find(|t| {
            t.split('-')
                .next()
                .unwrap_or(t)
                .eq_ignore_ascii_case(prefix)
        })
        .cloned()
}

/// The speech languages worth trying for one session, best first: the one `pick_speech_tag`
/// lands on, any other supported tag of the language the user chose, then Windows' own speech
/// language. A session tries them in turn, because a language Windows lists is not always one it
/// can open (a pack half installed, a model still downloading). Windows' own default is always
/// the last try, after these.
pub(crate) fn speech_candidates(
    chosen: Option<&str>,
    system: Option<&str>,
    supported: &[String],
) -> Vec<String> {
    let mut out: Vec<String> = Vec::new();
    let mut add = |tag: &str| {
        if !tag.is_empty() && !out.iter().any(|t| t.eq_ignore_ascii_case(tag)) {
            out.push(tag.to_owned());
        }
    };
    if let Some(chosen) = chosen {
        if let Some(tag) = pick_speech_tag(chosen, system.unwrap_or(""), supported) {
            add(&tag);
        }
        let prefix = chosen.split('-').next().unwrap_or(chosen);
        for tag in supported {
            if tag
                .split('-')
                .next()
                .unwrap_or(tag)
                .eq_ignore_ascii_case(prefix)
            {
                add(tag);
            }
        }
    }
    if let Some(system) = system {
        add(system);
    }
    out
}

// ---- the Windows speech service --------------------------------------------------------------

/// The speech service is spoken to from the session thread only, on a Runtime apartment of its
/// own. The three recognition events arrive on the engine's own threads and post into the
/// channel; the session thread reads them and talks to the page.
#[cfg(windows)]
struct WinRtEngine {
    tx: Sender<LiveMsg>,
    rx: Receiver<LiveMsg>,
    /// The speech language the user asked for, if any; `None` leaves it to Windows.
    language: Option<String>,
    inner: Option<Windows>,
    /// Balances the `RoInitialize` this session opened. Declared after `inner` so it drops after
    /// every interface: the Runtime must stay initialised until the last one is released.
    apartment: Option<UninitializeOnDrop>,
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

/// Releases the Runtime apartment when it drops.
#[cfg(windows)]
struct UninitializeOnDrop;

#[cfg(windows)]
impl Drop for UninitializeOnDrop {
    fn drop(&mut self) {
        unsafe { windows::Win32::System::WinRT::RoUninitialize() };
    }
}

#[cfg(windows)]
impl WinRtEngine {
    fn new(language: Option<String>) -> Self {
        let (tx, rx) = channel();
        Self {
            tx,
            rx,
            language,
            inner: None,
            apartment: None,
        }
    }
}

/// Windows' own speech language and the topic languages its service lists. Either is empty when
/// Windows will not say.
#[cfg(windows)]
fn speech_languages() -> (Option<String>, Vec<String>) {
    use windows::Media::SpeechRecognition::SpeechRecognizer;
    let system = SpeechRecognizer::SystemSpeechLanguage()
        .and_then(|l| l.LanguageTag())
        .map(|t| t.to_string())
        .ok();
    let mut supported: Vec<String> = Vec::new();
    if let Ok(view) = SpeechRecognizer::SupportedTopicLanguages() {
        for i in 0..view.Size().unwrap_or(0) {
            if let Some(tag) = view
                .GetAt(i)
                .and_then(|l| l.LanguageTag())
                .ok()
                .map(|t| t.to_string())
            {
                supported.push(tag);
            }
        }
    }
    (system, supported)
}

/// Every way to ask Windows for a recognizer, best first: the languages [`speech_candidates`]
/// picks, then Windows' own default (`None`). A session tries them in turn until one opens.
#[cfg(windows)]
fn candidate_languages(chosen: Option<&str>) -> Vec<Option<String>> {
    let (system, supported) = speech_languages();
    let mut out: Vec<Option<String>> = speech_candidates(chosen, system.as_deref(), &supported)
        .into_iter()
        .map(Some)
        .collect();
    out.push(None);
    out
}

/// One recognizer: in the named language, or in Windows' own default for `None`.
#[cfg(windows)]
fn create_recognizer(
    tag: Option<&str>,
) -> windows::core::Result<windows::Media::SpeechRecognition::SpeechRecognizer> {
    use windows::core::HSTRING;
    use windows::Media::SpeechRecognition::SpeechRecognizer;
    match tag {
        Some(tag) => {
            let lang = windows::Globalization::Language::CreateLanguage(&HSTRING::from(tag))?;
            SpeechRecognizer::Create(&lang)
        }
        None => SpeechRecognizer::new(),
    }
}

/// Give `recognizer` its dictation model and start a continuous session on it. The three
/// events post into `tx`.
#[cfg(windows)]
fn open_session(
    tx: &Sender<LiveMsg>,
    recognizer: &windows::Media::SpeechRecognition::SpeechRecognizer,
) -> windows::core::Result<Windows> {
    use windows::core::{Error as WError, Ref, HSTRING};
    use windows::Foundation::TypedEventHandler;
    use windows::Media::SpeechRecognition::{
        SpeechContinuousRecognitionCompletedEventArgs,
        SpeechContinuousRecognitionResultGeneratedEventArgs, SpeechContinuousRecognitionSession,
        SpeechRecognitionHypothesisGeneratedEventArgs, SpeechRecognitionResultStatus,
        SpeechRecognitionScenario, SpeechRecognitionTopicConstraint, SpeechRecognizer,
    };

    let hint = HSTRING::from("helm dictation");
    let constraint =
        SpeechRecognitionTopicConstraint::Create(SpeechRecognitionScenario::Dictation, &hint)?;
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

    let result_tx = tx.clone();
    let result_handler = TypedEventHandler::new(
        move |_s: Ref<SpeechContinuousRecognitionSession>,
              args: Ref<SpeechContinuousRecognitionResultGeneratedEventArgs>|
              -> windows::core::Result<()> {
            // A settled sentence; an empty one (nothing heard in this pause) still goes, so the
            // session clears the in-progress text of the dropped phrase.
            let text = args.ok()?.Result()?.Text()?;
            let _ = result_tx.send(LiveMsg::Final(text.to_string()));
            Ok(())
        },
    );
    let result_token = session.ResultGenerated(&result_handler)?;
    let completed_tx = tx.clone();
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
            let _ = completed_tx.send(msg);
            Ok(())
        },
    );
    let completed_token = session.Completed(&completed_handler)?;
    let hypothesis_tx = tx.clone();
    let hypothesis_handler = TypedEventHandler::new(
        move |_s: Ref<SpeechRecognizer>,
              args: Ref<SpeechRecognitionHypothesisGeneratedEventArgs>|
              -> windows::core::Result<()> {
            // Words as they are heard, for the sentence in progress. The service re-states the
            // whole sentence on every hypothesis, never just the new words, so every hypothesis
            // replaces the one before it: no shared state, nothing the thread ordering can
            // double up or drop.
            let text = args.ok()?.Hypothesis()?.Text()?;
            let t = text.to_string();
            if !t.trim().is_empty() {
                let _ = hypothesis_tx.send(LiveMsg::Replace(t));
            }
            Ok(())
        },
    );
    let hypothesis_token = recognizer.HypothesisGenerated(&hypothesis_handler)?;
    wait_action(&session.StartAsync()?, Duration::from_secs(10))?;
    Ok(Windows {
        recognizer: recognizer.clone(),
        session,
        result_token,
        completed_token,
        hypothesis_token,
        started: true,
    })
}

#[cfg(windows)]
impl Engine for WinRtEngine {
    fn start(&mut self) -> Result<(), String> {
        // SAFETY: this thread is owned by the session; RoInitialize runs once on it and the
        // guard below runs RoUninitialize after every interface is released. The recogniser is
        // agile, so the multithreaded apartment never shares COM pointers with other threads.
        unsafe {
            windows::Win32::System::WinRT::RoInitialize(
                windows::Win32::System::WinRT::RO_INIT_MULTITHREADED,
            )
            .map_err(|e| start_failure(&e))?;
        }
        // Declared before anything that needs the Runtime: it drops after all of it, on every
        // way out of this function. On success it moves into the engine.
        let apartment = UninitializeOnDrop;

        // The user's language when they named one, brought back to a tag this PC's speech
        // service really has (a bare `en` is not one of those), then the other tags of that
        // language, then Windows' own. The first that opens, compiles and starts wins; when none
        // does, the first failure is the one the user is told, because it is about the language
        // they asked for.
        let mut first_failure: Option<windows::core::Error> = None;
        for tag in candidate_languages(self.language.as_deref()) {
            let label = tag.as_deref().unwrap_or("the default language");
            let recognizer = match create_recognizer(tag.as_deref()) {
                Ok(r) => r,
                Err(e) => {
                    log::warn!("live dictation: opening {label} failed: {e}");
                    first_failure.get_or_insert(e);
                    continue;
                }
            };
            match open_session(&self.tx, &recognizer) {
                Ok(inner) => {
                    self.inner = Some(inner);
                    self.apartment = Some(apartment);
                    return Ok(());
                }
                Err(e) => {
                    log::warn!("live dictation: starting {label} failed: {e}");
                    recognizer.Close().ok();
                    first_failure.get_or_insert(e);
                }
            }
        }
        Err(first_failure.map_or_else(
            || "Windows speech recognition could not start on this PC.".to_owned(),
            |e| start_failure(&e),
        ))
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
        // The apartment is uninitialised by the engine's `apartment` field, which drops after
        // this one.
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
        // The generic "invalid operation" Windows answers when the speech service is there but
        // will not open a recogniser: in practice a speech language that is not installed (or
        // only half installed), or online speech recognition switched off.
        0x8013_1509 => {
            "Windows speech recognition would not start. Check that a speech language is installed in Windows Settings, under Time & language → Speech, and that online speech recognition is on under Privacy & security → Speech.".to_owned()
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
