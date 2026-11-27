//! An agent starting a Google sign-in: one Approve card, then Google's own page.
//!
//! The Gmail tools need a signed-in account, and a sign-in is the user's to make. When a chat
//! needs one (a first account, a personal Gmail, or an account that was signed in with some
//! Gmail boxes left unticked), the agent calls [`TOOL`] and Helm files one approval request
//! (allow kind [`KIND`]). Approving opens Google's consent page in the system browser on this
//! PC; the user only picks the account and allows it there. Helm keeps the tokens as for any
//! Settings sign-in, and the account belongs to the profile of the chat that asked.
//!
//! - **What is asked of Google.** Every Gmail permission, whatever the server's level is now, so
//!   raising the level later needs no second trip to Google's page. The level setting and the
//!   Allow card on each send stay what decide what a chat may do. When the server's Calendar
//!   level is on, the same page also asks for the calendar permissions of that level, and Helm
//!   hands the sign-in to the calendar bridge, so Calendar needs no page of its own. An account
//!   listed as needing a sign-in again is fixed with this same call.
//! - **The sign-in client stays the user's.** Its secret belongs to the profile and is entered
//!   in Settings; an agent cannot set it, and the refusal says so.
//! - **Finishing needs this PC.** Google sends the answer back to a loopback address, so the
//!   consent click happens in a browser on the PC Helm runs on. The answer to the agent says so.

use crate::config::McpStore;
use crate::google::{self, SignInScopes};
use crate::lines::ServicesCell;
use crate::types::McpServer;
use helm_consent::store::{self as consent, NewRequest};
use helm_consent::{AllowKind, AllowKinds, Applier, ApplyJob, Canonical};
use helm_core::db::Database;
use helm_hosttools::{HostToolRegistry, ToolCtx, ToolError, ToolResult, ToolSpec};
use helm_ipc::AppEvent;
use rusqlite::{OptionalExtension, Transaction};
use serde::{Deserialize, Serialize};
use serde_json::{json, Value};
use std::sync::Arc;

/// The allow kind of a sign-in an agent asked for.
pub const KIND: &str = "google_sign_in";
/// The tool that files it.
pub const TOOL: &str = "app_google_sign_in";

const MAX_NAME: usize = 80;
const MAX_REASON: usize = 1_000;
const NO_REASON: &str = "The agent needs a Google account signed in for Gmail.";

#[derive(Serialize, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
struct Target {
    /// The Google accounts server the sign-in belongs to.
    server: String,
    server_name: String,
    /// The account to pre-fill on Google's page; without one Google shows its chooser.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    email: Option<String>,
    /// The profile the account will belong to: the asking chat's own.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    profile_id: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    profile_name: Option<String>,
}

fn plain(text: &str, what: &str) -> Result<(), String> {
    if text.is_empty() || text.chars().count() > MAX_NAME || text.chars().any(char::is_control) {
        return Err(format!(
            "The {what} is not something a card can show whole."
        ));
    }
    Ok(())
}

fn canonical(_tx: &Transaction<'_>, _chat: &str, target: &Value) -> Result<Canonical, String> {
    let t: Target = serde_json::from_value(target.clone())
        .map_err(|_| format!("Ask for a Google sign-in with {TOOL}."))?;
    plain(&t.server, "server")?;
    plain(&t.server_name, "server name")?;
    if let Some(e) = &t.email {
        if !google::valid_email(e) || e.len() > MAX_NAME {
            return Err("That does not look like an email address.".into());
        }
    }
    if let Some(n) = &t.profile_name {
        plain(n, "profile name")?;
    }
    let account = match &t.email {
        Some(e) => format!("Account: {}", e.to_ascii_lowercase()),
        None => "Account: you choose it on Google's page".to_owned(),
    };
    let owner = match &t.profile_name {
        Some(n) => format!("For: {n}. Only this profile's chats can use the account."),
        None => "For: this profile. Only its chats can use the account.".to_owned(),
    };
    let details = vec![
        format!("Server: {}", t.server_name),
        account,
        owner,
        "Asks Google for: every Gmail permission (read, drafts, labels and changes, send), and the calendar ones when the server has Calendar on."
            .to_owned(),
        "What chats may do is still set by the server's level, and each send needs its own Allow."
            .to_owned(),
        "Approving opens Google's sign-in in the browser on this PC. You pick the account and allow it there."
            .to_owned(),
    ];
    Ok(Canonical {
        target: serde_json::to_value(&t).map_err(|e| e.to_string())?,
        summary: "Sign a Google account in to Helm".to_owned(),
        fixed_form: String::new(),
        details,
    })
}

fn kind() -> AllowKind {
    AllowKind {
        id: KIND,
        label: "Sign a Google account in",
        accepts_words: false,
        canonical,
    }
}

/// This module's allow kind, applied by Helm on Approve.
pub fn register_allow_kinds(k: &mut AllowKinds) {
    k.register_applying(kind(), Arc::new(SignInApplier));
}

fn refuse(m: impl Into<String>) -> ToolError {
    ToolError::refused(format!("[Helm] Nothing was staged: {}", m.into()))
}

/// The Google accounts server the call names (by id or name), or the only enabled one.
fn pick_server(list: &[McpServer], what: Option<&str>) -> Result<McpServer, String> {
    let google: Vec<&McpServer> = list
        .iter()
        .filter(|s| s.preset.as_deref() == Some(google::PRESET_ID))
        .collect();
    let found = match what {
        Some(w) => google
            .iter()
            .find(|s| s.id == w || s.name.eq_ignore_ascii_case(w))
            .copied()
            .ok_or_else(|| {
                "No Google accounts server has that id or name; list them with app_mcp_servers, or add one with app_mcp_servers add (preset google_accounts; if it is refused, send the same call with askUser: true)."
                    .to_owned()
            })?,
        None => {
            // A turned-off server does not make the choice ambiguous.
            let on: Vec<&&McpServer> = google.iter().filter(|s| s.enabled).collect();
            match (on.as_slice(), google.as_slice()) {
                ([one], _) => **one,
                ([], []) => {
                    return Err(
                        "No Google accounts server exists; add one with app_mcp_servers add (preset google_accounts; if it is refused, send the same call with askUser: true)."
                            .into(),
                    )
                }
                ([], [off, ..]) => *off,
                (many, _) => {
                    return Err(format!(
                        "More than one Google accounts server exists: say which with server ({}).",
                        many.iter()
                            .map(|s| s.id.as_str())
                            .collect::<Vec<_>>()
                            .join(", ")
                    ))
                }
            }
        }
    };
    if !found.enabled {
        return Err(format!(
            "{} is turned off; turn it on with app_mcp_servers set_enabled.",
            found.name
        ));
    }
    Ok(found.clone())
}

/// The profile's display name, when the profiles table has one for it.
fn profile_name(db: &Database, id: &str) -> Option<String> {
    db.snapshot(|tx| {
        Ok::<_, rusqlite::Error>(
            tx.query_row("SELECT name FROM profiles WHERE id = ?1", [id], |r| {
                r.get::<_, Option<String>>(0)
            })
            .optional()
            .unwrap_or_default()
            .flatten(),
        )
    })
    .ok()
    .flatten()
    .filter(|n| !n.trim().is_empty())
}

fn stage(ctx: &ToolCtx, args: Value) -> ToolResult {
    let text = |key: &str| -> Result<Option<String>, ToolError> {
        match args.get(key) {
            None | Some(Value::Null) => Ok(None),
            Some(Value::String(s)) if s.trim().is_empty() => Ok(None),
            Some(Value::String(s)) => Ok(Some(s.trim().to_owned())),
            Some(_) => Err(refuse(format!("{key} is text."))),
        }
    };
    let reason = match text("reason")? {
        Some(r) if r.chars().count() > MAX_REASON => {
            return Err(refuse(format!(
                "reason is at most {MAX_REASON} characters of text."
            )))
        }
        Some(r) => r,
        None => NO_REASON.to_owned(),
    };
    let email = match text("email")? {
        Some(e) if google::valid_email(&e) && e.len() <= MAX_NAME => Some(e.to_ascii_lowercase()),
        Some(_) => return Err(refuse("email does not look like an email address.")),
        None => None,
    };
    let (db, kinds, store) = match (
        ctx.services.get::<Database>(),
        ctx.services.get::<AllowKinds>(),
        ctx.services.get::<McpStore>(),
    ) {
        (Some(d), Some(k), Some(s)) if k.applies_on_allow(KIND) => (d, k, s),
        _ => return Err(refuse("a sign-in cannot be asked for here.")),
    };
    if !crate::repo_gate::all_on(&db) {
        return Err(refuse(
            "MCP is switched off (app_update_settings mcpEnabled turns it on), so the Google tools are off too.",
        ));
    }
    let list = store.load().map_err(|e| refuse(e.to_string()))?;
    let server = pick_server(&list, text("server")?.as_deref()).map_err(refuse)?;
    if !google::client_available(&store, &server) {
        return Err(refuse(
            "Helm has no Google sign-in client yet. Its secret is typed by the user, never through a chat: stage credentials {for: mcp} with app_stage_settings and Settings > MCP opens for the user to enter it (Google accounts, sign-in client), then ask again.",
        ));
    }
    if google::sign_in_open() {
        return Err(refuse(format!(
            "{} The user has a Google page open in the browser on this PC; once they finish it, ask again if the account is still missing.",
            google::SIGN_IN_OPEN
        )));
    }
    let profile_id = google::chat_profile(&db, &ctx.caller_chat_id);
    let profile_name = profile_id.as_deref().and_then(|p| profile_name(&db, p));
    let target = Target {
        server: server.id.clone(),
        server_name: server.name.clone(),
        email,
        profile_id,
        profile_name,
    };
    let target = serde_json::to_value(&target).map_err(|e| refuse(e.to_string()))?;
    let r = consent::request(
        &db,
        &kinds,
        NewRequest {
            chat_id: &ctx.caller_chat_id,
            kind: KIND,
            target: &target,
            reason: &reason,
            evidence: "",
            grant_minutes: None,
            tool_use_id: ctx.tool_call_id.as_deref(),
        },
        helm_core::now_ms(),
    )
    .map_err(|r| refuse(r.0))?;
    ctx.events
        .emit(AppEvent::new("consent").with("chatId", ctx.caller_chat_id.clone()));
    if let Some(w) = ctx.services.get::<helm_consent::ConsentWake>() {
        w.0.notify_one();
    }
    Ok(json!({
        "requestId": r.id,
        "state": "pending",
        "note": "The user now sees an Approve card in this chat. If they approve, Google's sign-in opens in the browser on the PC Helm runs on and they only have to pick the account and allow it there; if they are away from that PC it waits for them there. You get the outcome as a note. Do not call again or poll; check the accounts afterwards.",
    }))
}

/// The tool, listed to every chat: the first sign-in is needed before any Gmail tool is.
pub fn register_host_tools(r: &mut HostToolRegistry) {
    r.blocking(
        ToolSpec::write(
            TOOL,
            "Start signing a Google account in to Helm (for example a personal Gmail, a new account, or one whose Gmail permissions are missing). The user gets an Approve card; on Approve Google's own sign-in opens in the browser on this PC and they only pick the account and allow it. Every Gmail permission is asked for. Answers pending; the outcome reaches you as a note. It signs in with the client already saved in Settings > MCP and never changes that client.",
            json!({
                "type": "object",
                "properties": {
                    "server": {"type": "string", "description": "The Google accounts server's id or name; optional when there is exactly one."},
                    "email": {"type": "string", "description": "The account to pre-fill on Google's page, if you know it."},
                    "reason": {"type": "string", "minLength": 1, "maxLength": 1000}
                },
                "additionalProperties": false
            }),
        ),
        stage,
    );
}

/// Applies an approved sign-in request: opens Google's consent page on this PC.
pub struct SignInApplier;

impl Applier for SignInApplier {
    fn staged_with(&self) -> &'static str {
        "app_mcp_servers with action google_sign_in"
    }

    fn apply(&self, job: &ApplyJob<'_>) -> String {
        let Ok(t) = serde_json::from_value::<Target>(job.target.clone()) else {
            return "Not applied: the staged sign-in could not be read. Ask again.".into();
        };
        let Some(store) = job.services.get::<McpStore>() else {
            return "Not applied: MCP settings are not available.".into();
        };
        let Some(platform) = job.services.get::<helm_platform::Platform>() else {
            return "Not applied: this computer's browser is not available.".into();
        };
        let server = match store.load() {
            Ok(list) => list.into_iter().find(|s| {
                s.id == t.server && s.enabled && s.preset.as_deref() == Some(google::PRESET_ID)
            }),
            Err(e) => return format!("Not applied: {e}"),
        };
        let Some(server) = server else {
            return "Not applied: the Google accounts server is gone or turned off.".into();
        };
        let cell: Option<Arc<ServicesCell>> = job.services.get::<ServicesCell>();
        match google::begin_sign_in_with(
            &store,
            cell,
            platform.external.as_ref(),
            &server,
            t.email.as_deref(),
            t.profile_id.clone(),
            SignInScopes::AllGmail,
        ) {
            Ok(_) => "Applied. The Google sign-in is open in the browser on this PC. The user only has to pick the account and allow it there; if they are away it waits for them at the PC. Check the accounts again afterwards.".into(),
            Err(e) => format!("Not applied: {e}"),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn google_server(id: &str, name: &str, enabled: bool) -> McpServer {
        use crate::types::{Transport, TransportKind};
        McpServer {
            id: id.into(),
            name: name.into(),
            enabled,
            transport: Transport {
                kind: TransportKind::Stdio,
                command: Some("uvx.exe".into()),
                args: google::DEFAULT_ARGS.map(str::to_owned).to_vec(),
                env: Default::default(),
                url: None,
                headers: Default::default(),
                oauth: None,
            },
            allow_private_http: false,
            preset: Some(google::PRESET_ID.into()),
        }
    }

    /// Which server a call means: the only one by default, one named by id or name, and
    /// never a turned-off or missing one; the target's card lines name the account and the
    /// profile whole, and refuse what a card could not show whole.
    #[test]
    fn the_server_is_picked_and_the_card_says_whose_account_and_what_is_asked() {
        let one = vec![google_server("g1", "Google", true)];
        assert_eq!(pick_server(&one, None).unwrap().id, "g1");
        assert_eq!(pick_server(&one, Some("google")).unwrap().id, "g1");
        assert!(pick_server(&one, Some("nope")).is_err());
        assert!(pick_server(&[], None).is_err());
        let two = vec![
            google_server("g1", "Google", true),
            google_server("g2", "Work", true),
        ];
        let err = pick_server(&two, None).unwrap_err();
        assert!(err.contains("g1") && err.contains("g2"), "{err}");
        assert_eq!(pick_server(&two, Some("Work")).unwrap().id, "g2");
        let off = vec![google_server("g1", "Google", false)];
        assert!(pick_server(&off, None).unwrap_err().contains("turned off"));

        let tx_free = |t: Value| {
            let dir = tempfile::tempdir().unwrap();
            let db = Database::open(
                &dir.path().join("t.db"),
                &helm_core::db::MigrationRegistry::new(),
            )
            .unwrap();
            db.write::<_, rusqlite::Error>(|tx| Ok(canonical(tx, "c0", &t)))
                .unwrap()
        };
        let c = tx_free(json!({
            "server": "g1", "serverName": "Google", "email": "Me@Gmail.com",
            "profileId": "p1", "profileName": "Sam"
        }))
        .unwrap();
        assert_eq!(c.summary, "Sign a Google account in to Helm");
        assert!(c.details.iter().any(|l| l == "Account: me@gmail.com"));
        assert!(c.details.iter().any(|l| l.starts_with("For: Sam.")));
        assert!(c
            .details
            .iter()
            .any(|l| l.contains("every Gmail permission")));
        assert!(c.details.iter().all(|l| l.chars().count() <= 240));
        let any = tx_free(json!({"server": "g1", "serverName": "Google"})).unwrap();
        assert!(any.details.iter().any(|l| l.contains("you choose it")));
        for bad in [
            json!({"server": "g1", "serverName": "Google", "email": "not an address"}),
            json!({"server": "g1", "serverName": "Goo\ngle"}),
            json!({"server": "", "serverName": "Google"}),
            json!({"server": "g1", "serverName": "x".repeat(MAX_NAME + 1)}),
            json!({"server": "g1", "serverName": "Google", "extra": 1}),
            json!([]),
        ] {
            assert!(tx_free(bad.clone()).is_err(), "{bad}");
        }
    }

    /// Asking for a sign-in over a temp profile: refused while Helm has no sign-in client (in
    /// words that name the credentials page for it), refused for a server that is off or unknown or
    /// an address that is not one, and otherwise exactly one pending card that names the server,
    /// the account and the profile whole, and nothing is opened until the user approves.
    #[test]
    fn asking_files_one_card_and_refuses_what_an_agent_cannot_do() {
        use helm_core::db::MigrationRegistry;
        use helm_hosttools::CallOrigin;
        use helm_ipc::{NullSink, Services};
        // The sign-in flag is process-wide: wait for any sign-in test that holds it open.
        let _turn = crate::google::tests::AS_LOCK
            .lock()
            .unwrap_or_else(|p| p.into_inner());
        let dir = tempfile::tempdir().unwrap();
        let store = Arc::new(crate::config::tests::store(dir.path()));
        store
            .mutate(None, |l| {
                l.push(google_server("google", "Google", true));
                l.push(google_server("off", "Spare", false));
                Ok(())
            })
            .unwrap();
        let mut reg = MigrationRegistry::new();
        helm_consent::register_migrations(&mut reg);
        let db = Arc::new(Database::open(&dir.path().join("t.db"), &reg).unwrap());
        db.write::<_, rusqlite::Error>(|tx| {
            tx.execute(
                "INSERT INTO projects(id, name, created_at) VALUES ('p', 'P', 1)",
                [],
            )?;
            tx.execute_batch(
                "CREATE TABLE profiles(id TEXT PRIMARY KEY, name TEXT, is_home INTEGER);
                 INSERT INTO profiles VALUES ('home', 'Sam', 1);
                 ALTER TABLE chats ADD COLUMN profile_id TEXT;
                 INSERT INTO chats(id, project_id, created_at, updated_at) VALUES ('c0', 'p', 1, 1);",
            )
        })
        .unwrap();
        let mut kinds = AllowKinds::new();
        register_allow_kinds(&mut kinds);
        let kinds = Arc::new(kinds);
        let mut services = Services::new();
        services.insert(store.clone());
        services.insert(db.clone());
        services.insert(kinds.clone());
        let ctx = ToolCtx {
            caller_chat_id: "c0".into(),
            is_child: false,
            tool_call_id: None,
            origin: CallOrigin::Agent,
            services: Arc::new(services),
            events: Arc::new(NullSink),
            ops: None,
        };

        let err = stage(&ctx, json!({})).unwrap_err().text();
        assert!(
            err.contains("Settings > MCP") && err.contains("credentials"),
            "{err}"
        );
        google::seed_client(store.creds());
        for (args, want) in [
            (json!({"server": "off"}), "turned off"),
            (json!({"server": "nope"}), "No Google accounts server"),
            (json!({"email": "not an address"}), "email"),
            (json!({"reason": "r".repeat(MAX_REASON + 1)}), "reason"),
        ] {
            let err = stage(&ctx, args.clone()).unwrap_err().text();
            assert!(err.contains(want), "{args}: {err}");
        }
        let out = stage(
            &ctx,
            json!({"email": "Me@Gmail.com", "reason": "the user wants their mail in Helm"}),
        )
        .unwrap();
        assert_eq!(out["state"], "pending");
        assert!(
            !out.to_string().contains("gmail.com"),
            "the answer holds no card line"
        );
        let card = helm_consent::store::get(&db, &kinds, out["requestId"].as_str().unwrap())
            .unwrap()
            .unwrap();
        assert_eq!(card.kind, KIND);
        assert_eq!(card.chat_id, "c0");
        assert!(
            card.summary.contains("Account: me@gmail.com"),
            "{}",
            card.summary
        );
        assert!(card.summary.contains("Server: Google"), "{}", card.summary);
        assert!(card.summary.contains("For: Sam."), "{}", card.summary);
    }
}
