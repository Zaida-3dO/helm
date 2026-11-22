//! Approving one browser step from wherever the user is.
//!
//! The built-in browser server refuses a consequential step on the user's own browser with
//! `attached.needs_owner`. For a step on one control (press, tick, choose, type) the chat's
//! worker reads what the control is (its accessible name and role, the site's host name, a
//! picture of it) and asks the host here to show an Approve card. Running one expression in the
//! page is asked the same way: the card shows the whole expression and the site's host name, and
//! the grant is signed over an empty control reference and name plus a digest of the exact
//! expression. Nothing the agent holds can approve anything:
//!
//! - **The card** is a consent kind Helm applies itself ([`KIND`]). It is answered in Helm's
//!   window, from the user's own other devices, or with the Approve button on their own Telegram,
//!   and the first answer wins. A household member's device never answers it, and neither does
//!   anyone's Telegram but the chat owner's: those are the rules for every card Helm applies.
//! - **On Approve** the host mints a one-time grant: HMAC-SHA256, under a key Helm makes at start
//!   and keeps only in memory, over the lease, the control's reference, its accessible name, the
//!   site's host name, the second it was made, the approved verb, and a digest of the value the
//!   step carries (a grant for "type 3" is not a grant for "type 3000"). The broker accepts it
//!   once, within two minutes, for exactly that tuple, and refuses it on any other lease or
//!   control. The key reaches the
//!   broker in its own environment ([`SECRET_ENV`]) and joins the worker's redaction list like
//!   every resolved secret; it is in no log, op result or tool answer.
//! - **The grant never passes through the agent.** It goes to the chat's worker as a line on its
//!   stdin ([`GRANT_LINE`]); the worker retries the one step with it and hands back what the
//!   browser answered ([`RESULT_TOOL`]), and that answer reaches the agent as the card's outcome.
//! - **Never asked.** A checkout, an order, a payment, a transfer or closing an account is always
//!   the user's own step, and signing in or a password, one-time code or card field stays at their
//!   browser. Both are refused before any card exists and checked again when applying. The word
//!   lists are shared with the worker (`worker/src/mcp/owner-step-words.json`).
//!
//! Both tools are the worker's own: never listed to an agent, and refused over the relay.

use crate::config::McpStore;
use crate::presets::BROKER_PRESET_ID;
use base64::Engine as _;
use helm_consent::store::{self as consent, NewRequest};
use helm_consent::{AllowKind, AllowKinds, Applier, ApplyJob, Canonical};
use helm_hosttools::{
    Broadcaster, Condition, HostToolRegistry, ToolCtx, ToolError, ToolResult, ToolSpec,
};
use hmac::{Hmac, Mac};
use rusqlite::Transaction;
use serde::{Deserialize, Serialize};
use serde_json::{json, Value};
use sha2::{Digest as _, Sha256};
use std::collections::HashMap;
use std::sync::{mpsc, Arc, Mutex, OnceLock};
use std::time::Duration;

/// The consent kind of a browser step.
pub const KIND: &str = "browser_step";
/// The worker's tool that files the card.
pub const STAGE_TOOL: &str = "app_browser_step";
/// The worker's tool that hands back what the browser answered to the approved step.
pub const RESULT_TOOL: &str = "app_browser_step_result";
/// The worker line that carries a grant.
pub const GRANT_LINE: &str = "mcp_owner_grant";
/// The broker's variable for the key it checks grants with.
pub const SECRET_ENV: &str = "BROKER_OWNER_GRANT_SECRET";
/// A card answered later than this is not applied: the worker stops holding the tab by then, and
/// the page has likely moved on.
pub const STEP_LIFE_MS: i64 = 15 * 60_000;
/// How long applying waits for the browser's answer.
pub const ANSWER_WAIT: Duration = Duration::from_secs(60);
/// The broker verbs a grant can answer for: those that act on one named control, and `evaluate`,
/// which runs one expression in the page and names no control.
pub const VERBS: &[&str] = &[
    "click", "check", "select", "type", "fill", "press", "evaluate",
];
/// The verb that runs one expression in the page.
const EVALUATE: &str = "evaluate";
/// The longest expression the broker accepts, in bytes of source.
const MAX_EXPRESSION_BYTES: usize = 4096;
/// The longest row of the code on a card: a longer line continues on the next row.
const CODE_ROW: usize = 220;

/// The refusal for a step that is always the user's own.
pub const NEVER: &str = "This step is one the user always does themselves: Helm never approves a checkout, an order, a payment, a transfer or closing an account on their behalf, so no card was shown. Leave the browser where it is and tell the user it is ready for them.";
/// The refusal for a sign-in or a secret field.
pub const SECRET: &str = "This step signs in or touches a password, one-time code or card field, which stay with the user at their browser, so no card was shown.";

/// The refusal for code a card could not show faithfully.
const UNREADABLE_CODE: &str = "The code holds characters a person cannot see on a card (control or invisible ones), so no card was shown.";

/// The key grants are signed with: made once per start, kept only in memory. `None` only when
/// the system gave no randomness, and then no grant is ever made.
pub fn grant_key() -> Option<&'static str> {
    static KEY: OnceLock<Option<String>> = OnceLock::new();
    KEY.get_or_init(|| {
        let mut b = [0u8; 32];
        getrandom::fill(&mut b)
            .ok()
            .map(|_| base64::engine::general_purpose::URL_SAFE_NO_PAD.encode(b))
    })
    .as_deref()
}

/// What a grant answers for.
pub struct Tuple<'a> {
    pub lease_key: &'a str,
    pub reference: &'a str,
    pub name: &'a str,
    pub host: &'a str,
    pub issued_at: u64,
    /// The verb the owner approved: a grant for "click Pay" is not a grant for `type` on the
    /// same control.
    pub verb: &'a str,
    /// The value the step carries (the key, the text, the option), if any.
    pub value: Option<&'a str>,
}

/// The digest of the value the call carries, as the broker binds it: hex SHA-256, and the empty
/// string for a verb that carries none. The value itself never travels in a grant.
fn value_digest(value: Option<&str>) -> String {
    match value {
        None | Some("") => String::new(),
        Some(v) => sha2::Sha256::digest(v.as_bytes())
            .iter()
            .map(|b| format!("{b:02x}"))
            .collect(),
    }
}

/// A grant in the broker's wire form, `<issued-at>.<base64url tag>`. The fields are joined with
/// `|`, each with backslash and pipe escaped, so a page cannot make two tuples sign alike by
/// putting a pipe in a control's name.
pub fn mint(key: &str, t: &Tuple<'_>) -> String {
    let esc = |s: &str| s.replace('\\', "\\\\").replace('|', "\\|");
    let payload = format!(
        "{}|{}|{}|{}|{}|{}|{}",
        esc(t.lease_key),
        esc(t.reference),
        esc(t.name),
        esc(t.host),
        t.issued_at,
        esc(t.verb),
        esc(&value_digest(t.value))
    );
    let mut mac = Hmac::<Sha256>::new_from_slice(key.as_bytes()).expect("HMAC takes any key");
    mac.update(payload.as_bytes());
    let tag = base64::engine::general_purpose::URL_SAFE_NO_PAD.encode(mac.finalize().into_bytes());
    format!("{}.{tag}", t.issued_at)
}

#[derive(Deserialize)]
struct WordClass {
    words: Vec<String>,
    pairs: Vec<(String, String)>,
}

#[derive(Deserialize)]
struct Words {
    never: WordClass,
    secret: WordClass,
}

fn words() -> &'static Words {
    static W: OnceLock<Words> = OnceLock::new();
    W.get_or_init(|| {
        serde_json::from_str(include_str!(
            "../../../../worker/src/mcp/owner-step-words.json"
        ))
        .expect("the shared word list parses")
    })
}

/// Characters a reader never sees: zero-width joins and spaces, the bidi controls, the soft
/// hyphen, and combining marks (so an accent cannot split or hide a word).
fn invisible(c: char) -> bool {
    matches!(c as u32,
        0x00AD | 0x200B..=0x200F | 0x202A..=0x202E | 0x2060 | 0xFEFF | 0x0300..=0x036F)
}

/// One character folded the way the word lists are written: fullwidth forms to plain ones and
/// the common accented Latin letters to their base letter.
fn fold(c: char) -> char {
    let c = match c as u32 {
        0xFF01..=0xFF5E => char::from_u32(c as u32 - 0xFEE0).unwrap_or(c),
        _ => c,
    };
    match c {
        'à'..='å' | 'À'..='Å' | 'ā' | 'Ā' => 'a',
        'ç' | 'Ç' => 'c',
        'è'..='ë' | 'È'..='Ë' | 'ē' | 'Ē' => 'e',
        'ì'..='ï' | 'Ì'..='Ï' | 'ī' | 'Ī' => 'i',
        'ñ' | 'Ñ' => 'n',
        'ò'..='ö' | 'Ò'..='Ö' | 'ø' | 'Ø' | 'ō' | 'Ō' => 'o',
        'ù'..='ü' | 'Ù'..='Ü' | 'ū' | 'Ū' => 'u',
        'ý' | 'ÿ' | 'Ý' => 'y',
        other => other,
    }
}

/// Lower-case word tokens of a control's name or role.
pub fn tokens(s: &str) -> Vec<String> {
    let mut out = Vec::new();
    let mut run = String::new();
    for c in s.chars().filter(|c| !invisible(*c)).map(fold) {
        if c.is_alphanumeric() {
            run.extend(c.to_lowercase());
        } else if !run.is_empty() {
            out.push(std::mem::take(&mut run));
        }
    }
    if !run.is_empty() {
        out.push(run);
    }
    out
}

fn hits(class: &WordClass, seen: &[String]) -> bool {
    let has = |w: &str| seen.iter().any(|t| t == w);
    class.words.iter().any(|w| has(w)) || class.pairs.iter().any(|(a, b)| has(a) && has(b))
}

/// Where a step stands before any card.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum StepClass {
    /// Always the user's own: refused outright.
    Never,
    /// A sign-in or a secret field: back to the user at their browser.
    Secret,
    /// An Approve card may be shown.
    Ask,
}

/// The class of a step on a control named `name` with role `role`. `value` is what a type,
/// fill or choice would enter; `page_secret` says the page shows a sign-in or card field.
pub fn class_of(name: &str, role: &str, value: Option<&str>, page_secret: bool) -> StepClass {
    let mut seen = tokens(name);
    seen.extend(tokens(role));
    if hits(&words().never, &seen) {
        return StepClass::Never;
    }
    let stored_secret = value.is_some_and(|v| v.contains("{{secret:"));
    if hits(&words().secret, &seen) || stored_secret || page_secret {
        return StepClass::Secret;
    }
    StepClass::Ask
}

/// One staged step: the card's target. Built by the staging tool from what the worker read, and
/// never from the agent's words.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct Step {
    pub server_id: String,
    pub lease_key: String,
    #[serde(rename = "ref")]
    pub reference: String,
    pub verb: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub value: Option<String>,
    /// The control's accessible name as the page reports it, exactly (the grant signs it).
    pub name: String,
    #[serde(default)]
    pub role: String,
    /// The page's host name.
    pub host: String,
    #[serde(default)]
    pub page_secret: bool,
    #[serde(default)]
    pub picture: bool,
    pub staged_at: i64,
}

fn plain_text(s: &str, max: usize) -> String {
    let flat: String = s
        .chars()
        .filter(|c| !invisible(*c))
        .map(|c| if c.is_control() { ' ' } else { c })
        .collect();
    let flat = flat.split_whitespace().collect::<Vec<_>>().join(" ");
    let mut it = flat.chars();
    let cut: String = it.by_ref().take(max).collect();
    if it.next().is_some() {
        format!("{cut}…")
    } else {
        cut
    }
}

fn host_ok(h: &str) -> bool {
    (1..=253).contains(&h.len())
        && h.bytes()
            .all(|b| b.is_ascii_lowercase() || b.is_ascii_digit() || b == b'.' || b == b'-')
}

/// The card's rows for an expression. Every row starts with `| ` (a long line continues on a row
/// that starts `|+ `), so no line of the code can pass for one of the card's own lines.
fn code_rows(code: &str) -> Vec<String> {
    let mut rows = Vec::new();
    for line in code.split('\n') {
        let line = line.strip_suffix('\r').unwrap_or(line);
        let chars: Vec<char> = line.chars().collect();
        if chars.is_empty() {
            rows.push("|".to_owned());
        }
        for (i, part) in chars.chunks(CODE_ROW).enumerate() {
            let mark = if i == 0 { "| " } else { "|+ " };
            rows.push(format!("{mark}{}", part.iter().collect::<String>()));
        }
    }
    rows
}

/// The detail lines of a card for an expression, in the order the card shows them.
fn evaluate_details(s: &Step) -> Vec<String> {
    let mut details = vec![
        format!("The site: {}", s.host),
        "It can read and change whatever this page shows. You will not see what it returns."
            .to_owned(),
        "The code, whole:".to_owned(),
    ];
    details.extend(code_rows(s.value.as_deref().unwrap_or("")));
    details.push("Approving runs exactly this, once, within two minutes.".to_owned());
    details
}

/// The rules for an expression step: the exact text, shown whole on the card, and no control.
fn check_expression(s: &Step) -> Result<(), String> {
    if !s.reference.is_empty() || !s.name.is_empty() {
        return Err("Running code names no control, so a card for it carries none.".into());
    }
    let Some(code) = s.value.as_deref().filter(|v| !v.trim().is_empty()) else {
        return Err("The step says nothing about what it runs.".into());
    };
    if code.len() > MAX_EXPRESSION_BYTES {
        return Err(format!(
            "The code is longer than the browser accepts ({MAX_EXPRESSION_BYTES} bytes)."
        ));
    }
    if code.contains("{{secret:") {
        return Err(SECRET.into());
    }
    let odd_control = |c: char| c.is_control() && !matches!(c, '\n' | '\t' | '\r');
    if code.chars().any(|c| invisible(c) || odd_control(c))
        || code.replace("\r\n", "").contains('\r')
    {
        return Err(UNREADABLE_CODE.into());
    }
    // The card keeps each line to 240 characters and the whole to 1,200; code that would be cut
    // is refused rather than shown in part.
    let title_len = title(s).chars().count();
    let rows = evaluate_details(s);
    let fits = title_len <= helm_consent::kinds::MAX_SUMMARY_LINE
        && rows
            .iter()
            .all(|l| l.chars().count() <= helm_consent::kinds::MAX_SUMMARY_LINE)
        && title_len + rows.iter().map(|l| l.chars().count() + 1).sum::<usize>()
            <= helm_consent::kinds::MAX_SUMMARY;
    if !fits {
        return Err(format!(
            "The code is too long to show whole on one card (the card holds {} characters in all): ask for a shorter expression.",
            helm_consent::kinds::MAX_SUMMARY
        ));
    }
    Ok(())
}

/// Every rule a step must pass, at staging and again when applying. `Err` is the refusal.
pub fn check(s: &Step) -> Result<(), String> {
    if !VERBS.contains(&s.verb.as_str()) {
        return Err(format!(
            "A card can approve only a step on one control or one expression ({}).",
            VERBS.join(", ")
        ));
    }
    if s.server_id.is_empty() || s.server_id.len() > 64 {
        return Err("The step names no browser server.".into());
    }
    if !(1..=200).contains(&s.lease_key.chars().count())
        || s.lease_key.chars().any(char::is_control)
    {
        return Err("The step names no lease.".into());
    }
    if s.verb == EVALUATE {
        if !host_ok(&s.host) {
            return Err("The step names no site.".into());
        }
        // No word lists: the card shows the code itself and the user judges it.
        return check_expression(s);
    }
    if !(1..=32).contains(&s.reference.len())
        || !s
            .reference
            .bytes()
            .all(|b| b.is_ascii_alphanumeric() || b == b'_' || b == b'-')
    {
        return Err("The step names no control reference.".into());
    }
    if !(1..=300).contains(&s.name.chars().count()) || plain_text(&s.name, 300).is_empty() {
        return Err("The control has no name the user could approve by.".into());
    }
    if !host_ok(&s.host) {
        return Err("The step names no site.".into());
    }
    let needs_value = matches!(s.verb.as_str(), "select" | "type" | "fill" | "press");
    match &s.value {
        Some(v) if v.chars().count() > 300 => {
            return Err("What the step would enter is longer than a card shows.".into())
        }
        None if needs_value => return Err("The step says nothing about what it enters.".into()),
        _ => {}
    }
    match class_of(&s.name, &s.role, s.value.as_deref(), s.page_secret) {
        StepClass::Never => Err(NEVER.into()),
        StepClass::Secret => Err(SECRET.into()),
        StepClass::Ask => Ok(()),
    }
}

/// The card's title line.
pub fn title(s: &Step) -> String {
    let name = plain_text(&s.name, 120);
    let host = plain_text(&s.host, 120);
    let value = plain_text(s.value.as_deref().unwrap_or(""), 120);
    match s.verb.as_str() {
        "check" => format!("Tick \"{name}\" on {host}"),
        "select" => format!("Choose \"{value}\" in \"{name}\" on {host}"),
        "type" | "fill" => format!("Type \"{value}\" into \"{name}\" on {host}"),
        "press" => format!("Press the {value} key in \"{name}\" on {host}"),
        EVALUATE => format!(
            "Run code on {host}: \"{}\"",
            plain_text(s.value.as_deref().unwrap_or(""), 60)
        ),
        _ => format!("Press \"{name}\" on {host}"),
    }
}

fn canonical(_tx: &Transaction<'_>, _chat: &str, t: &Value) -> Result<Canonical, String> {
    let s: Step = serde_json::from_value(t.clone())
        .map_err(|_| "The browser step could not be read.".to_owned())?;
    check(&s)?;
    if s.verb == EVALUATE {
        return Ok(Canonical {
            target: serde_json::to_value(&s).map_err(|e| e.to_string())?,
            summary: title(&s),
            fixed_form: String::new(),
            details: evaluate_details(&s),
        });
    }
    let mut details = Vec::new();
    if !s.role.trim().is_empty() {
        details.push(format!(
            "The control: {} \"{}\"",
            plain_text(&s.role, 40),
            plain_text(&s.name, 160)
        ));
    }
    details.push(format!("The site: {}", s.host));
    details.push(if s.picture {
        "A picture of the control is on this card in Helm.".to_owned()
    } else {
        "No picture of the control could be taken.".to_owned()
    });
    details.push(
        "Approving lets Helm make this one step in your browser, once, within two minutes. It allows nothing else."
            .to_owned(),
    );
    Ok(Canonical {
        target: serde_json::to_value(&s).map_err(|e| e.to_string())?,
        summary: title(&s),
        fixed_form: String::new(),
        details,
    })
}

/// This module's allow kind, applied by Helm on Approve.
pub fn register_allow_kinds(k: &mut AllowKinds) {
    k.register_applying(
        AllowKind {
            id: KIND,
            label: "Approve a step in your browser",
            accepts_words: false,
            canonical,
        },
        Arc::new(StepApplier { wait: ANSWER_WAIT }),
    );
}

/// One waiting answer: the chat it belongs to, and the channel its worker will answer on.
type StepWaiter = (String, mpsc::Sender<(bool, String)>);

/// The browser's answers to approved steps, waited for by request id. Each waiter remembers its
/// chat, so only that chat's worker can answer it.
#[derive(Default)]
pub struct StepAnswers(Mutex<HashMap<String, StepWaiter>>);

impl StepAnswers {
    fn map(&self) -> std::sync::MutexGuard<'_, HashMap<String, StepWaiter>> {
        self.0.lock().unwrap_or_else(|p| p.into_inner())
    }

    fn expect(&self, request_id: &str, chat_id: &str) -> mpsc::Receiver<(bool, String)> {
        let (tx, rx) = mpsc::channel();
        self.map()
            .insert(request_id.to_owned(), (chat_id.to_owned(), tx));
        rx
    }

    fn forget(&self, request_id: &str) {
        self.map().remove(request_id);
    }

    /// Hand an answer to the step `request_id` of `chat_id`. False when nothing of that chat waits.
    pub fn answer(&self, request_id: &str, chat_id: &str, ok: bool, text: String) -> bool {
        let mut m = self.map();
        match m.get(request_id) {
            Some((chat, _)) if chat == chat_id => {
                let (_, tx) = m.remove(request_id).expect("present");
                tx.send((ok, text)).is_ok()
            }
            _ => false,
        }
    }
}

/// Applies an approved step: mints the grant, sends it to the chat's worker and waits for the
/// browser's answer. It changes nothing of Helm's own, so it holds no maintenance lock.
pub struct StepApplier {
    pub wait: Duration,
}

impl Applier for StepApplier {
    fn staged_with(&self) -> &'static str {
        STAGE_TOOL
    }

    fn holds_maintenance_lock(&self) -> bool {
        false
    }

    fn apply(&self, job: &ApplyJob<'_>) -> String {
        let Ok(s) = serde_json::from_value::<Step>(job.target.clone()) else {
            return "Not done: the approved step could not be read, and nothing was pressed in the browser.".into();
        };
        if let Err(why) = check(&s) {
            return format!("Not done: {why}");
        }
        let now = helm_core::now_ms();
        if now - s.staged_at > STEP_LIFE_MS {
            return "Not done: the approval came more than 15 minutes after the step was asked for, and the page has likely moved on. Read the page again and ask once more if the step is still wanted.".into();
        }
        let Some(key) = grant_key() else {
            return "Not done: Helm could not make an approval key this time it started, so nothing was pressed in the browser.".into();
        };
        let (Some(answers), Some(workers)) = (
            job.services.get::<StepAnswers>(),
            job.services.get::<Broadcaster>(),
        ) else {
            return "Not done: the chat could not be reached, so nothing was pressed in the browser.".into();
        };
        let waiting = answers.expect(job.request_id, job.chat_id);
        let issued_at = u64::try_from(now / 1000).unwrap_or(0);
        let grant = mint(
            key,
            &Tuple {
                lease_key: &s.lease_key,
                reference: &s.reference,
                name: &s.name,
                host: &s.host,
                issued_at,
                verb: &s.verb,
                value: s.value.as_deref(),
            },
        );
        let line = json!({
            "type": GRANT_LINE,
            "requestId": job.request_id,
            "serverId": s.server_id,
            "leaseKey": s.lease_key,
            "ref": s.reference,
            "grant": grant,
        });
        if !workers.0.send_to_worker(job.chat_id, &line) {
            answers.forget(job.request_id);
            return "Not done: the chat is not running, so nothing was pressed in the browser."
                .into();
        }
        let step = title(&s);
        match waiting.recv_timeout(self.wait) {
            Ok((true, text)) => format!(
                "Done: {step}. The browser answered: {}",
                plain_text(&text, 1_500)
            ),
            Ok((false, text)) => format!(
                "Not done: {step}. The browser refused the approved step: {}",
                plain_text(&text, 1_500)
            ),
            Err(_) => {
                answers.forget(job.request_id);
                format!("Unknown: Helm passed the approval for \"{step}\" to the browser but heard nothing back within a minute. Read the page before doing anything else.")
            }
        }
    }
}

fn text_arg<'a>(a: &'a Value, k: &str) -> Option<&'a str> {
    a.get(k).and_then(Value::as_str)
}

/// The server the worker names must be an enabled built-in browser server.
fn browser_server(ctx: &ToolCtx, id: &str) -> Result<(), ToolError> {
    let store = ctx
        .services
        .get::<McpStore>()
        .ok_or_else(|| ToolError::refused("MCP settings are not available."))?;
    let ok = store
        .load()
        .map_err(|e| ToolError::refused(e.to_string()))?
        .iter()
        .any(|s| s.id == id && s.enabled && s.preset.as_deref() == Some(BROKER_PRESET_ID));
    if ok {
        Ok(())
    } else {
        Err(ToolError::refused(
            "That server is not an enabled built-in browser.",
        ))
    }
}

/// The step the worker asks for, checked: an enabled built-in browser server and every rule of
/// [`check`]. Nothing is stored before this passes.
fn staged_step(ctx: &ToolCtx, a: &Value) -> Result<Step, ToolError> {
    let server_id = text_arg(a, "serverId").unwrap_or("").to_owned();
    browser_server(ctx, &server_id)?;
    let step = Step {
        server_id,
        lease_key: text_arg(a, "leaseKey").unwrap_or("").to_owned(),
        reference: text_arg(a, "ref").unwrap_or("").to_owned(),
        verb: text_arg(a, "verb").unwrap_or("").to_owned(),
        value: text_arg(a, "value").map(str::to_owned),
        name: text_arg(a, "name").unwrap_or("").to_owned(),
        role: text_arg(a, "role").unwrap_or("").to_owned(),
        host: text_arg(a, "host").unwrap_or("").to_owned(),
        page_secret: a.get("pageSecret").and_then(Value::as_bool) == Some(true),
        picture: false,
        staged_at: helm_core::now_ms(),
    };
    check(&step).map_err(|why| ToolError::refused(format!("[Helm] {why}")))?;
    Ok(step)
}

/// File the card for a checked step, with the picture Helm stored for it if any.
fn file_step(
    ctx: &ToolCtx,
    mut step: Step,
    gallery: Option<String>,
    purpose: Option<&str>,
) -> ToolResult {
    let (db, kinds) = match (
        ctx.services.get::<helm_core::db::Database>(),
        ctx.services.get::<AllowKinds>(),
    ) {
        (Some(d), Some(k)) => (d, k),
        _ => return Err(ToolError::refused("Approve cards are not available here.")),
    };
    step.picture = gallery.is_some();
    // The lease's purpose is the agent's own words: the card shows it as such.
    let purpose = purpose
        .map(|p| plain_text(p, 900))
        .filter(|p| !p.is_empty())
        .unwrap_or_else(|| "The chat gave no purpose for its browser lease.".into());
    let target = serde_json::to_value(&step).map_err(|e| ToolError::refused(e.to_string()))?;
    let r = consent::request(
        &db,
        &kinds,
        NewRequest {
            chat_id: &ctx.caller_chat_id,
            kind: KIND,
            target: &target,
            reason: &purpose,
            evidence: "",
            grant_minutes: None,
            tool_use_id: None,
        },
        helm_core::now_ms(),
    )
    .map_err(|r| ToolError::refused(format!("[Helm] {}", r.0)))?;
    if let Some(g) = &gallery {
        let _ = consent::set_picture(&db, &kinds, &r.id, g);
    }
    ctx.events
        .emit(helm_ipc::AppEvent::new("consent").with("chatId", ctx.caller_chat_id.clone()));
    if let Some(w) = ctx.services.get::<helm_consent::ConsentWake>() {
        w.0.notify_one();
    }
    Ok(json!({
        "requestId": r.id,
        "state": "pending",
        "summary": consent::summary_title(&r.summary),
    }))
}

/// [`STAGE_TOOL`]: the worker files the Approve card for one step it was refused.
async fn stage(ctx: ToolCtx, a: Value) -> ToolResult {
    let step = staged_step(&ctx, &a)?;
    // Helm keeps its own copy of the picture in the chat, so the card shows what was there when
    // it was asked, whatever happens to the broker's file.
    let mut gallery = None;
    if let Some(path) = text_arg(&a, "picture").filter(|p| !p.trim().is_empty()) {
        let args = json!({
            "chatId": ctx.caller_chat_id,
            "title": "The control to approve",
            "images": [{"path": path, "caption": plain_text(&step.name, 200)}],
        });
        if let Ok(made) = ctx.op("shown_images_add", args).await {
            gallery = made
                .get("galleryId")
                .and_then(Value::as_str)
                .map(str::to_owned);
        }
    }
    file_step(&ctx, step, gallery, text_arg(&a, "purpose"))
}

/// [`RESULT_TOOL`]: the worker hands back what the browser answered to an approved step.
fn result(ctx: &ToolCtx, a: Value) -> ToolResult {
    let id = text_arg(&a, "requestId").unwrap_or("");
    let ok = a.get("ok").and_then(Value::as_bool) == Some(true);
    let text = text_arg(&a, "text").unwrap_or("").to_owned();
    let delivered = ctx
        .services
        .get::<StepAnswers>()
        .is_some_and(|w| w.answer(id, &ctx.caller_chat_id, ok, text));
    if delivered {
        Ok(json!({ "received": true }))
    } else {
        Err(ToolError::refused(
            "No approved step of this chat waits for that answer.",
        ))
    }
}

pub fn register_host_tools(r: &mut HostToolRegistry) {
    r.async_tool(
        ToolSpec::write(
            STAGE_TOOL,
            "Worker-only: files the Approve card for one browser step the user's own browser refused as theirs to approve. Chats and agents never see this tool.",
            json!({
                "type": "object",
                "properties": {
                    "serverId": {"type": "string"},
                    "leaseKey": {"type": "string"},
                    "ref": {"type": "string"},
                    "verb": {"type": "string"},
                    "value": {"type": "string"},
                    "name": {"type": "string"},
                    "role": {"type": "string"},
                    "host": {"type": "string"},
                    "purpose": {"type": "string"},
                    "picture": {"type": "string"},
                    "pageSecret": {"type": "boolean"}
                },
                "required": ["serverId", "leaseKey", "verb", "host"],
                "additionalProperties": false
            }),
        )
        .when(Condition::Worker)
        .timeout_ms(30_000),
        stage,
    );
    r.blocking(
        ToolSpec::read(
            RESULT_TOOL,
            "Worker-only: hands back what the browser answered to a step the user approved. Chats and agents never see this tool.",
            json!({
                "type": "object",
                "properties": {
                    "requestId": {"type": "string"},
                    "ok": {"type": "boolean"},
                    "text": {"type": "string", "maxLength": 8000}
                },
                "required": ["requestId", "ok", "text"],
                "additionalProperties": false
            }),
        )
        .when(Condition::Worker),
        result,
    );
}

#[cfg(test)]
mod tests;
