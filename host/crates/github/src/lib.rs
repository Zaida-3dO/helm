//! GitHub for the pull request view.
//!
//! Helm talks to github.com through the GitHub command-line tool (`gh api`) under the user's own
//! sign-in: Helm holds no GitHub credential, never asks `gh` for its token and never reads its
//! configuration. The land step and declared waits already rely on `gh` the same way.
//!
//! The view has one user-only switch, off by default. While it is off nothing here runs: every
//! network op checks the switch before anything is spawned. The ops are main-window ops only;
//! none of them is an agent tool.
//!
//! | op | args → answer |
//! |---|---|
//! | `github_settings_get` | `{}` → `{enabled, revision}` |
//! | `github_settings_save` | `{enabled, expectedRevision}` → `{enabled, revision}`; refused unless `expectedRevision` is the stored revision |
//! | `github_viewer` | `{}` → answer, `data: {login, name, avatarUrl}` ("Check sign-in") |
//! | `github_rest` | `{path, query?}` → answer; GET only, allowlisted routes ([`rest`]) |
//! | `github_graphql` | `{id, variables?}` → answer; compiled documents only ([`graphql`]) |
//! | `github_search_prs` | `{query}` → answer for `is:pr involves:@me <query>`, 10 rows |
//! | `github_comment` | `{owner, repo, number, body}` → answer (the new comment) |
//! | `github_review_comment` | `{owner, repo, number, head, path, line, side, startLine?, body}` → answer |
//! | `github_review_reply` | `{owner, repo, number, threadId, body}` → answer (`data.comment`) |
//! | `github_review_resolve` | `{threadId, resolve}` → answer (`data.thread`, `data.changed`) |
//! | `github_review` | `{owner, repo, number, head, event, body}` → answer (the new review) |
//!
//! An answer is `{status, rateLimit, nextPage?, data, errors?}`: `rateLimit` is
//! `{limit, remaining, used, resetAt}` (ms since the epoch) when GitHub sent it, `nextPage` the
//! next REST page number (1–1000), `errors` GraphQL's errors next to partial data. A failure is
//! an error whose message is `[Class] sentence` ([`error::Class`]). Saving the settings emits
//! `github_settings`.
//!
//! Debug builds only: `HELM_TEST_GITHUB_URL` (a loopback `http://` address) replaces `gh` with
//! plain HTTP to a fake: REST at `<url>/<path>`, GraphQL at `POST <url>/graphql`.

pub mod client;
pub mod error;
pub mod graphql;
pub mod rest;
pub mod transport;
pub mod writes;

#[cfg(test)]
mod tests;

use client::{Answer, Client};
use error::{Class, GhError, GhResult};
use helm_core::db::Database;
use helm_core::prefs::{self, PrefError, PrefKey};
use helm_ipc::{ApiError, AppEvent, EventSink, OpClass, OpCtx, OpRegistry, Services};
use serde::de::DeserializeOwned;
use serde::{Deserialize, Serialize};
use serde_json::{json, Value};
use std::sync::Arc;
use transport::{Method, Request, Transport};

pub const PR_VIEW: PrefKey = PrefKey::classified("githubPrView");
/// Rows `github_search_prs` asks for.
pub const SEARCH_ROWS: u64 = 10;
const MAX_REVISION: u64 = (1 << 53) - 1;

/// The pull request view switch.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct Settings {
    pub enabled: bool,
    pub revision: u64,
}

#[derive(Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
struct SaveArgs {
    enabled: bool,
    expected_revision: u64,
}

#[derive(Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
struct RestArgs {
    path: String,
    #[serde(default)]
    query: Option<Value>,
}

#[derive(Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
struct GraphqlArgs {
    id: String,
    #[serde(default)]
    variables: Option<Value>,
}

#[derive(Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
struct SearchArgs {
    query: String,
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct NoArgs {}

/// The shared client; one per app.
pub struct GitHub {
    pub client: Client,
}

impl GitHub {
    pub fn new(transport: Arc<dyn Transport>) -> Self {
        Self {
            client: Client::new(transport, Arc::new(helm_core::now_ms)),
        }
    }

    pub fn viewer(&self) -> GhResult<Answer> {
        let mut a = self.graphql("viewer", None)?;
        a.data = a.data["viewer"].clone();
        Ok(a)
    }

    pub fn rest(&self, path: &str, query: Option<&Value>) -> GhResult<Answer> {
        let endpoint = rest::endpoint(path, query)?;
        self.client.read(&Request {
            method: Method::Get,
            endpoint,
            body: None,
        })
    }

    /// A compiled document the renderer may name.
    pub fn graphql(&self, id: &str, variables: Option<&Value>) -> GhResult<Answer> {
        let doc = graphql::doc(id)
            .ok_or_else(|| GhError::invalid("There is no such GitHub request in Helm."))?;
        if doc.native {
            return Err(GhError::new(
                Class::Refused,
                "That GitHub request is made only by Helm itself.",
            ));
        }
        let req = Request {
            method: Method::Post,
            endpoint: "graphql".into(),
            body: Some(graphql::body(doc, variables)?),
        };
        if graphql::is_mutation(doc) {
            // The field a mutation document calls is named as the document is.
            let mut a = self.client.mutate(&req, doc.id)?;
            a.data = json!({ doc.id: a.data });
            Ok(a)
        } else {
            self.client.read(&req)
        }
    }

    pub fn search_prs(&self, text: &str) -> GhResult<Answer> {
        let text: String = text
            .chars()
            .map(|c| if c.is_control() { ' ' } else { c })
            .collect();
        let text = text.trim();
        if text.chars().count() > 256 {
            return Err(GhError::invalid(
                "Search text is limited to 256 characters.",
            ));
        }
        let q = format!("is:pr involves:@me {text}");
        self.graphql(
            "searchPullRequests",
            Some(&json!({"q": q.trim_end(), "first": SEARCH_ROWS})),
        )
    }
}

// ---- text hygiene -----------------------------------------------------------------------

/// Remove anything shaped like a GitHub token from text Helm shows or logs.
pub fn scrub(text: &str) -> String {
    const PREFIXES: [&str; 6] = ["github_pat_", "ghp_", "gho_", "ghu_", "ghs_", "ghr_"];
    let mut out = String::with_capacity(text.len());
    let mut rest = text;
    'outer: while !rest.is_empty() {
        for p in PREFIXES {
            if let Some(after) = rest.strip_prefix(p) {
                let n = after
                    .bytes()
                    .take_while(|b| b.is_ascii_alphanumeric() || *b == b'_')
                    .count();
                if n >= 16 {
                    out.push_str("[token]");
                    rest = &after[n..];
                    continue 'outer;
                }
            }
        }
        let c = rest.chars().next().expect("non-empty");
        out.push(c);
        rest = &rest[c.len_utf8()..];
    }
    out
}

/// One line of at most `max` characters: control characters become spaces, tokens are scrubbed.
pub fn clean(text: &str, max: usize) -> String {
    let one: String = scrub(text)
        .chars()
        .map(|c| if c.is_control() { ' ' } else { c })
        .collect();
    let one = one.trim();
    if one.chars().count() <= max {
        return one.to_owned();
    }
    let mut cut: String = one.chars().take(max.saturating_sub(1)).collect();
    cut.push('…');
    cut
}

// ---- settings -----------------------------------------------------------------------------

/// The stored switch; anything unreadable reads as off.
pub fn read_settings(conn: &rusqlite::Connection) -> Settings {
    prefs::get_or(conn, PR_VIEW, Settings::default())
}

fn settings(db: &Database) -> Settings {
    db.snapshot(|tx| Ok::<_, rusqlite::Error>(read_settings(tx)))
        .unwrap_or_default()
}

/// Compare-and-swap: refused unless `expected` is the stored revision; the new revision is
/// `expected + 1`.
pub fn save_settings(db: &Database, enabled: bool, expected: u64) -> GhResult<Settings> {
    let out = db.write(|tx| -> Result<GhResult<Settings>, PrefError> {
        let cur = read_settings(tx);
        if cur.revision != expected || expected >= MAX_REVISION {
            return Ok(Err(GhError::new(
                Class::Refused,
                "The GitHub settings changed meanwhile; they have been reloaded, try again.",
            )));
        }
        let next = Settings {
            enabled,
            revision: expected + 1,
        };
        prefs::set(tx, PR_VIEW, &serde_json::to_value(next).expect("plain"))?;
        Ok(Ok(next))
    });
    out.map_err(|e| {
        GhError::new(
            Class::Refused,
            format!("The settings could not be saved: {e}"),
        )
    })?
}

// ---- ops ----------------------------------------------------------------------------------

pub fn register_ops(reg: &mut OpRegistry) {
    reg.blocking("github_settings_get", OpClass::Blocking, settings_get)
        .blocking("github_settings_save", OpClass::Maintenance, settings_save)
        .blocking("github_viewer", OpClass::Blocking, |ctx, args| {
            parse::<NoArgs>(args)?;
            answer(gate(ctx)?.viewer())
        })
        .blocking("github_rest", OpClass::Blocking, |ctx, args| {
            let a: RestArgs = parse(args)?;
            answer(gate(ctx)?.rest(&a.path, a.query.as_ref()))
        })
        .blocking("github_graphql", OpClass::Blocking, |ctx, args| {
            let a: GraphqlArgs = parse(args)?;
            answer(gate(ctx)?.graphql(&a.id, a.variables.as_ref()))
        })
        .blocking("github_search_prs", OpClass::Blocking, |ctx, args| {
            let a: SearchArgs = parse(args)?;
            answer(gate(ctx)?.search_prs(&a.query))
        })
        .blocking("github_comment", OpClass::Blocking, |ctx, args| {
            let a = parse(args)?;
            answer(writes::comment(&gate(ctx)?.client, &a))
        })
        .blocking("github_review_comment", OpClass::Blocking, |ctx, args| {
            let a = parse(args)?;
            answer(writes::review_comment(&gate(ctx)?.client, &a))
        })
        .blocking("github_review_reply", OpClass::Blocking, |ctx, args| {
            let a = parse(args)?;
            answer(writes::review_reply(&gate(ctx)?.client, &a))
        })
        .blocking("github_review_resolve", OpClass::Blocking, |ctx, args| {
            let a = parse(args)?;
            answer(writes::review_resolve(&gate(ctx)?.client, &a))
        })
        .blocking("github_review", OpClass::Blocking, |ctx, args| {
            let a = parse(args)?;
            answer(writes::review(&gate(ctx)?.client, &a))
        });
}

/// The shared client. Debug builds use the loopback transport when `HELM_TEST_GITHUB_URL` is set.
pub fn register_services(services: &mut Services, _events: &Arc<dyn EventSink>) {
    let transport: Arc<dyn Transport> = match transport::Loopback::from_env() {
        Some(t) => Arc::new(t),
        None => Arc::new(transport::GhCli),
    };
    services.insert(Arc::new(GitHub::new(transport)));
}

fn parse<T: DeserializeOwned>(args: Value) -> Result<T, ApiError> {
    let args = if args.is_null() { json!({}) } else { args };
    serde_json::from_value(args).map_err(|e| {
        GhError::invalid(format!("Bad arguments: {}", clean(&e.to_string(), 200))).into()
    })
}

fn answer(r: GhResult<Answer>) -> Result<Value, ApiError> {
    let a = r?;
    serde_json::to_value(a).map_err(|e| ApiError::internal(e.to_string()))
}

/// The client, only while the view is on.
fn gate(ctx: &OpCtx) -> Result<Arc<GitHub>, ApiError> {
    let db = ctx.services.need::<Database>()?;
    if !settings(&db).enabled {
        return Err(GhError::new(
            Class::Disabled,
            "The pull request view is off. Turn it on in Settings, GitHub.",
        )
        .into());
    }
    ctx.services.need::<GitHub>()
}

fn settings_get(ctx: &OpCtx, args: Value) -> Result<Value, ApiError> {
    parse::<NoArgs>(args)?;
    let db = ctx.services.need::<Database>()?;
    Ok(serde_json::to_value(settings(&db)).expect("plain"))
}

fn settings_save(ctx: &OpCtx, args: Value) -> Result<Value, ApiError> {
    let a: SaveArgs = parse(args)?;
    let db = ctx.services.need::<Database>()?;
    let saved = save_settings(&db, a.enabled, a.expected_revision)?;
    if !saved.enabled {
        if let Some(gh) = ctx.services.get::<GitHub>() {
            gh.client.clear_pause();
        }
    }
    ctx.events
        .emit(AppEvent::new("github_settings").with("enabled", saved.enabled));
    Ok(serde_json::to_value(saved).expect("plain"))
}
