//! A rule or a skill saved into another person's own profile, on the user's Approve.
//!
//! An agent may save its own person's knowledge on that person's say-so ([`crate::tools`]), but
//! never another person's. When it names another profile (`person`), nothing is written: one
//! Approve card is filed in the asking chat instead. The card names whose profile it is and shows
//! the exact text of the rule, or the folder, description and file count of the skill. Helm writes
//! it into that person's layer only when the user approves, and only if the rule or folder is
//! still what the card showed. The other person does not confirm anything. The change reaches
//! that person's other PCs through the ordinary knowledge sync when their profile is linked to
//! the hub.
//!
//! The card shows the whole text or is not filed: a text too long for a card is refused, and the
//! agent is told so.

use crate::launch::ChatKnowledge;
use crate::layers::{self, Kind, Layer};
use crate::undo::{self, Restore};
use crate::{rules, tools};
use helm_consent::kinds::{MAX_SUMMARY, MAX_SUMMARY_LINE};
use helm_consent::{AllowKind, AllowKinds, Applier, ApplyJob, Canonical, NewRequest};
use helm_core::db::Database;
use helm_core::profile::Profile;
use helm_hosttools::registry::{CallOrigin, ToolCtx, ToolError, ToolResult};
use helm_ipc::{AppEvent, EventSink, Services};
use rusqlite::{OptionalExtension, Transaction};
use serde::{Deserialize, Serialize};
use serde_json::{json, Value};
use sha2::{Digest, Sha256};
use std::path::Path;
use std::sync::Arc;

const KIND: &str = "knowledge_for_person";
const STAGED_WITH: &str = "app_save_rule or app_add_skill with person";
/// Rule text is shown in lines of at most this many characters (a card line holds 240).
const TEXT_LINE: usize = 230;

/// Another person's profile on this PC.
#[derive(Clone, Debug)]
pub(crate) struct Person {
    id: String,
    name: String,
    /// Linked to a person on the hub, so their layer syncs to their other PCs.
    linked: bool,
}

/// Who a `person` argument names.
pub(crate) enum Named {
    /// The chat's own profile: the same as `scope: me`.
    Own,
    Other(Person),
}

#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
struct RuleChange {
    title: String,
    text: String,
    always: Option<bool>,
    /// The rule this one rewrites (`rules/<file>.md`), with its title and revision as read.
    replace: Option<Replaced>,
}

#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
struct Replaced {
    path: String,
    title: String,
    revision: String,
}

#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
struct SkillChange {
    source: String,
    name: String,
    description: String,
    files: usize,
    /// A hash of every file's path and contents, as read when the card was filed.
    digest: String,
}

#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
struct Staged {
    profile_id: String,
    profile_name: String,
    linked: bool,
    /// The person the asking chat belongs to, for the outcome line.
    asked_by: String,
    rule: Option<RuleChange>,
    skill: Option<SkillChange>,
}

// ---- the card -----------------------------------------------------------------------------

/// Split `s` into lines of at most `max` characters, breaking at its own line breaks first.
/// Blank lines are left out: a card line is never empty.
fn wrap(s: &str, max: usize) -> Vec<String> {
    let mut out = Vec::new();
    for line in s.lines() {
        let line = line.trim();
        let chars: Vec<char> = line.chars().collect();
        for part in chars.chunks(max) {
            let part: String = part.iter().collect();
            let part = part.trim();
            if !part.is_empty() {
                out.push(part.to_owned());
            }
        }
    }
    out
}

fn yes_no(b: bool) -> &'static str {
    if b {
        "yes"
    } else {
        "no"
    }
}

/// The card's lines, each shown whole. A card that would not fit is refused rather than cut.
fn card_lines(s: &Staged) -> Result<Vec<String>, String> {
    let name = &s.profile_name;
    let mut lines = Vec::new();
    match (&s.rule, &s.skill) {
        (Some(r), None) => {
            lines.push(format!("Save a rule for {name}"));
            lines.push(format!("This goes into {name}'s own profile, not yours."));
            if !s.linked {
                lines.push(format!(
                    "{name} is not linked to the hub, so this applies on this PC only."
                ));
            }
            lines.push(format!("Title: {}", r.title));
            let always = match r.always {
                Some(a) => yes_no(a),
                None if r.replace.is_some() => "unchanged",
                None => "no",
            };
            lines.push(format!("Kept whole in every chat: {always}"));
            if let Some(p) = &r.replace {
                lines.push(format!("Replaces the rule \"{}\" ({})", p.title, p.path));
            }
            lines.push("Text:".into());
            lines.extend(wrap(&r.text, TEXT_LINE));
        }
        (None, Some(k)) => {
            lines.push(format!("Add the skill {} for {name}", k.name));
            lines.push(format!("This goes into {name}'s own profile, not yours."));
            if !s.linked {
                lines.push(format!(
                    "{name} is not linked to the hub, so this applies on this PC only."
                ));
            }
            lines.push(format!("From the folder: {}", k.source));
            lines.push("Description:".into());
            lines.extend(wrap(&k.description, TEXT_LINE));
            lines.push(format!("Files: {}", k.files));
        }
        _ => return Err("Stage a rule or a skill, not both or neither.".into()),
    }
    if lines.iter().any(|l| l.chars().count() > MAX_SUMMARY_LINE) {
        return Err(format!(
            "too long to show whole: a line of the card passes {MAX_SUMMARY_LINE} characters. \
             Save a shorter one, or {name} adds it in Settings."
        ));
    }
    let total = lines.join("\n").chars().count();
    if total > MAX_SUMMARY {
        return Err(format!(
            "too long to show whole ({total} characters): save a shorter rule, or {name} adds it in Settings."
        ));
    }
    Ok(lines)
}

fn canonical(_tx: &Transaction<'_>, _chat: &str, target: &Value) -> Result<Canonical, String> {
    let staged: Staged = serde_json::from_value(target.clone())
        .map_err(|_| "Stage this with app_save_rule or app_add_skill and a person.".to_owned())?;
    let lines = card_lines(&staged)?;
    Ok(Canonical {
        target: serde_json::to_value(&staged).map_err(|e| e.to_string())?,
        summary: lines.join("\n"),
        fixed_form: String::new(),
        details: Vec::new(),
    })
}

fn kind() -> AllowKind {
    AllowKind {
        id: KIND,
        label: "Save knowledge into another person's profile",
        accepts_words: false,
        canonical,
    }
}

/// This module's allow kind, applied by Helm on Approve.
pub fn register_allow_kinds(k: &mut AllowKinds) {
    k.register_applying(kind(), Arc::new(KnowledgeApplier));
}

// ---- staging ------------------------------------------------------------------------------

/// Resolve `person` (a profile name or id on this PC). The chat's own profile is [`Named::Own`].
pub(crate) fn resolve(
    ctx: &ToolCtx,
    who: &ChatKnowledge,
    person: &str,
) -> Result<Named, ToolError> {
    let person = person.trim();
    if person.is_empty() {
        return Err(ToolError::refused("person is a profile name or id."));
    }
    if let Some(own) = &who.profile {
        if own.id == person || own.name.eq_ignore_ascii_case(person) {
            return Ok(Named::Own);
        }
    }
    let db = ctx
        .services
        .get::<Database>()
        .ok_or_else(|| ToolError::refused("Helm has no database here."))?;
    let found = db
        .snapshot(|tx| {
            // The id first, then the name (names are unique without regard to case).
            let row = |r: &rusqlite::Row<'_>| {
                Ok::<_, rusqlite::Error>(Person {
                    id: r.get(0)?,
                    name: r.get(1)?,
                    linked: r
                        .get::<_, Option<String>>(2)?
                        .is_some_and(|p| !p.is_empty()),
                })
            };
            let by_id = tx
                .query_row(
                    "SELECT id, name, hub_person_id FROM profiles WHERE id = ?1",
                    [person],
                    row,
                )
                .optional()?;
            match by_id {
                Some(p) => Ok(Some(p)),
                None => tx
                    .query_row(
                        "SELECT id, name, hub_person_id FROM profiles
                         WHERE name = ?1 COLLATE NOCASE",
                        [person],
                        row,
                    )
                    .optional(),
            }
        })
        .map_err(|e| ToolError::refused(format!("Helm could not look the person up: {e}")))?;
    match found {
        Some(p) if layers::valid_profile_id(&p.id) => Ok(Named::Other(p)),
        _ => Err(ToolError::refused(
            "No person with that name or id has a profile on this PC.",
        )),
    }
}

fn digest_hex(parts: impl IntoIterator<Item = Vec<u8>>) -> String {
    let mut h = Sha256::new();
    for p in parts {
        h.update((p.len() as u64).to_le_bytes());
        h.update(&p);
    }
    hex::encode(h.finalize())
}

/// A hash of a skill folder: every file's path and contents.
fn folder_digest(dir: &Path) -> std::io::Result<(usize, String)> {
    let files = crate::skill_files(dir)?;
    let mut parts = Vec::new();
    for f in &files {
        parts.push(f.rel.clone().into_bytes());
        parts.push(helm_core::privfs::read_capped(
            &f.path,
            crate::MAX_SKILL_FILE,
        )?);
    }
    Ok((files.len(), digest_hex(parts)))
}

fn hidden_in(s: &str) -> bool {
    helm_core::hidden::find_hidden(&Value::String(s.to_owned())).is_some()
}

fn reason_of(a: &Value) -> String {
    a.get("reason")
        .and_then(Value::as_str)
        .map(str::trim)
        .filter(|r| !r.is_empty())
        .unwrap_or("The user asked to save this for another person.")
        .to_owned()
}

fn asked_by(who: &ChatKnowledge) -> String {
    who.profile
        .as_ref()
        .map(|p| p.name.clone())
        .unwrap_or_else(|| "a chat with no person".into())
}

/// Stage `rule` for `person`: file the card, write nothing.
pub(crate) fn stage_rule(
    ctx: &ToolCtx,
    who: &ChatKnowledge,
    person: Person,
    a: &Value,
) -> ToolResult {
    let text_of = |key: &str| {
        a.get(key)
            .and_then(Value::as_str)
            .ok_or_else(|| ToolError::refused(format!("{key} is required.")))
    };
    let title = text_of("title")?.trim().to_owned();
    let text = text_of("text")?.trim().to_owned();
    if title.is_empty() || title.chars().count() > rules::MAX_TITLE_CHARS {
        return Err(ToolError::refused("A rule's title is 1 to 120 characters."));
    }
    if helm_core::hidden::has_hidden(&title) || hidden_in(&text) {
        return Err(ToolError::refused(
            "The rule cannot hold control, invisible or direction-changing characters.",
        ));
    }
    if text.is_empty() || text.len() > rules::MAX_AGENT_RULE_TEXT {
        return Err(ToolError::refused(
            "A rule needs some text, and not too much.",
        ));
    }
    let data = tools::profile_dir(ctx)?;
    let root = Layer::Profile(person.id.clone()).root(&data);
    let replace = match a.get("replace").and_then(Value::as_str) {
        Some(path) => {
            let (entry, _) = rules::read(&root, path).map_err(|_| {
                ToolError::refused(format!("{} has no rule {path} to replace.", person.name))
            })?;
            Some(Replaced {
                path: entry.path,
                title: entry.title,
                revision: entry.revision,
            })
        }
        None => None,
    };
    let staged = Staged {
        profile_id: person.id.clone(),
        profile_name: person.name.clone(),
        linked: person.linked,
        asked_by: asked_by(who),
        rule: Some(RuleChange {
            title,
            text,
            always: a.get("always").and_then(Value::as_bool),
            replace,
        }),
        skill: None,
    };
    file(ctx, &staged, &reason_of(a))
}

/// Stage `skill` for `person`: file the card, write nothing.
pub(crate) fn stage_skill(
    ctx: &ToolCtx,
    who: &ChatKnowledge,
    person: Person,
    a: &Value,
) -> ToolResult {
    let path = a
        .get("path")
        .and_then(Value::as_str)
        .ok_or_else(|| ToolError::refused("path is required."))?;
    let data = tools::profile_dir(ctx)?;
    let source =
        helm_platform::pathgate::check(path).map_err(|r| ToolError::refused(r.message()))?;
    if helm_platform::private::PrivateRoots::new(std::slice::from_ref(&data)).contains(&source) {
        return Err(ToolError::refused(
            "Helm's own data folder cannot be used as a skill source.",
        ));
    }
    let root = Layer::Profile(person.id.clone()).root(&data);
    let name = source
        .file_name()
        .map(|s| s.to_string_lossy().into_owned())
        .unwrap_or_default();
    if !crate::is_valid_name(&name) {
        return Err(ToolError::refused(
            "The folder's name must be lower-case letters, digits and hyphens.",
        ));
    }
    if crate::is_inside(&source, &crate::skills_dir(&root)) || crate::is_inside(&root, &source) {
        return Err(ToolError::refused(
            "Pick a folder outside the skills folder.",
        ));
    }
    if crate::skills_dir(&root).join(&name).exists() {
        return Err(ToolError::refused(format!(
            "{} already has a skill called {name}.",
            person.name
        )));
    }
    let (files, digest) = folder_digest(&source)
        .map_err(|e| ToolError::refused(format!("The skill folder cannot be used: {e}")))?;
    let md = ["SKILL.md", "skill.md", "Skill.md"]
        .iter()
        .map(|n| source.join(n))
        .find(|p| p.is_file())
        .ok_or_else(|| ToolError::refused("The folder has no SKILL.md."))?;
    let description = crate::front_description(&md);
    let source_text = source.to_string_lossy().into_owned();
    if helm_core::hidden::has_hidden(&source_text) || hidden_in(&description) {
        return Err(ToolError::refused(
            "The folder's path or description holds control, invisible or direction-changing characters.",
        ));
    }
    let staged = Staged {
        profile_id: person.id.clone(),
        profile_name: person.name.clone(),
        linked: person.linked,
        asked_by: asked_by(who),
        rule: None,
        skill: Some(SkillChange {
            source: source_text,
            name,
            description,
            files,
            digest,
        }),
    };
    file(ctx, &staged, &reason_of(a))
}

fn file(ctx: &ToolCtx, staged: &Staged, reason: &str) -> ToolResult {
    let refuse = |m: String| ToolError::refused(format!("[Helm] Nothing was saved or staged: {m}"));
    card_lines(staged).map_err(refuse)?;
    let (db, kinds) = match (
        ctx.services.get::<Database>(),
        ctx.services.get::<AllowKinds>(),
    ) {
        (Some(d), Some(k)) if k.applies_on_allow(KIND) => (d, k),
        _ => return Err(refuse("this is not available here.".into())),
    };
    let target = serde_json::to_value(staged).map_err(|e| ToolError::refused(e.to_string()))?;
    let r = helm_consent::store::request(
        &db,
        &kinds,
        NewRequest {
            chat_id: &ctx.caller_chat_id,
            kind: KIND,
            target: &target,
            reason,
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
        "note": format!(
            "Nothing was saved yet. The user sees an Approve card in this chat showing exactly what would go into {}'s own profile. Helm saves it only if they approve, and tells you the outcome in a note. Do not call again or poll.",
            staged.profile_name
        ),
    }))
}

// ---- applying -----------------------------------------------------------------------------

/// Applies an approved `knowledge_for_person` request.
pub struct KnowledgeApplier;

impl Applier for KnowledgeApplier {
    fn staged_with(&self) -> &'static str {
        STAGED_WITH
    }

    fn apply(&self, job: &ApplyJob<'_>) -> String {
        match serde_json::from_value::<Staged>(job.target.clone()) {
            Ok(s) => apply_staged(job.services, job.events, job.chat_id, &s),
            Err(_) => "Nothing saved: the request could not be read.".into(),
        }
    }
}

fn apply_staged(
    services: &Arc<Services>,
    events: &Arc<dyn EventSink>,
    chat_id: &str,
    s: &Staged,
) -> String {
    let nothing = |why: &str| format!("Nothing saved: {why}");
    let Some(profile) = services.get::<Profile>() else {
        return nothing("Helm has no profile folder here.");
    };
    let data = profile.dir.clone();
    let still_there = services.get::<Database>().is_some_and(|db| {
        db.snapshot(|tx| {
            tx.query_row(
                "SELECT name FROM profiles WHERE id = ?1",
                [&s.profile_id],
                |r| r.get::<_, String>(0),
            )
            .optional()
        })
        .ok()
        .flatten()
        .is_some_and(|n| n == s.profile_name)
    });
    if !still_there {
        return nothing(&format!(
            "{} is no longer a profile on this PC under that name. Ask again.",
            s.profile_name
        ));
    }
    let layer = Layer::Profile(s.profile_id.clone());
    let root = layer.root(&data);
    let before = undo::digest(&data);
    let ctx = ToolCtx {
        caller_chat_id: chat_id.to_owned(),
        is_child: false,
        tool_call_id: None,
        origin: CallOrigin::Agent,
        services: services.clone(),
        events: events.clone(),
        ops: None,
    };
    let (restore, said, path) = match (&s.rule, &s.skill) {
        (Some(r), None) => {
            let replaced = match &r.replace {
                Some(p) => {
                    match rules::read(&root, &p.path) {
                        Ok((now, _)) if now.revision == p.revision => {}
                        _ => {
                            return nothing(&format!(
                                "the rule {} changed since the card was made. Ask again.",
                                p.path
                            ))
                        }
                    }
                    match undo::stash(&data, &layer, Kind::Rule, &p.path) {
                        Ok(x) => Some(x),
                        Err(e) => return nothing(&format!("the old rule could not be kept: {e}")),
                    }
                }
                None => None,
            };
            let written = rules::write(
                &root,
                r.replace.as_ref().map(|p| p.path.as_str()),
                &r.title,
                &r.text,
                r.replace.as_ref().map(|p| p.revision.as_str()),
                r.always,
                rules::MAX_AGENT_RULE_TEXT,
            );
            let entry = match written {
                Ok(e) => e,
                Err(e) => {
                    if let Some(x) = &replaced {
                        undo::discard(&data, x);
                    }
                    return nothing(&e.to_string());
                }
            };
            let restore = replaced.unwrap_or_else(|| Restore {
                layer: layer.clone(),
                kind: Kind::Rule,
                name: entry.path.trim_start_matches("rules/").to_owned(),
                trash: None,
            });
            (
                restore,
                format!(
                    "Saved the rule {} to {}'s rules",
                    entry.title, s.profile_name
                ),
                entry.path,
            )
        }
        (None, Some(k)) => {
            let source = Path::new(&k.source);
            match folder_digest(source) {
                Ok((_, d)) if d == k.digest => {}
                _ => {
                    return nothing(&format!(
                        "the folder {} changed since the card was made. Ask again.",
                        k.source
                    ))
                }
            }
            let added = match crate::add(&root, source) {
                Ok(a) => a,
                Err(e) => return nothing(&e.to_string()),
            };
            (
                Restore {
                    layer: layer.clone(),
                    kind: Kind::Skill,
                    name: added.name.clone(),
                    trash: None,
                },
                format!(
                    "Added the skill {} to {}'s skills",
                    added.name, s.profile_name
                ),
                format!("skills/{}", added.name),
            )
        }
        _ => return nothing("the request names neither a rule nor a skill."),
    };
    // The text is never logged: who asked, whose profile, and where.
    log::info!(
        "knowledge for another person: chat {chat_id} saved {path} into profile {}",
        s.profile_id
    );
    let undo = tools::file_change(&ctx, &data, &before, &restore)
        .map(|_| " The user was shown a notice with Undo.")
        .unwrap_or("");
    let sync = if s.linked {
        ""
    } else {
        " It applies on this PC only."
    };
    format!("Applied. {said} (asked by {}).{sync}{undo}", s.asked_by)
}
