//! A rule, a skill or a memory saved for another person, by naming them.
//!
//! Personal knowledge is shown and applied only for the person it was made for, and that is all
//! it is: a chat of any person can write into another person's layer by naming them (`person`),
//! exactly as it writes into its own. The change is made at once and filed in the change ring, so
//! the user is shown a notice with Undo. The other person confirms nothing. The change reaches
//! that person's other PCs through the ordinary knowledge sync when their profile is linked to
//! the hub.
//!
//! A memory for another person lives on the hub, not in a profile folder. Helm saves it there as
//! that person's memory (shared with the household), sending the hub a header that names the
//! person it is for. The hub keeps who saved it. A hub too old to honour that is refused before
//! anything is written, because an old hub would save the text as the asker's own.

use crate::launch::ChatKnowledge;
use crate::layers::{self, Kind, Layer};
use crate::memories::{HubMemory, MemError, MemoryHub, MemoryHubHandle};
use crate::undo::{self, Restore};
use crate::{rules, tools};
use helm_core::db::Database;
use helm_core::profile::Profile;
use helm_hosttools::registry::{CallOrigin, ToolCtx, ToolError, ToolResult};
use helm_ipc::{EventSink, Services};
use rusqlite::OptionalExtension;
use serde::{Deserialize, Serialize};
use serde_json::{json, Value};
use sha2::{Digest, Sha256};
use std::path::Path;
use std::sync::Arc;

/// Tags on a memory saved for someone else.
const MAX_TAGS: usize = 8;
const MAX_TAG_CHARS: usize = 40;
const MAX_MEMORY_TITLE: usize = 120;
/// Words that mean the one speaking, which a memory for someone else should not use.
const FIRST_PERSON: &[&str] = &[
    "my", "mine", "me", "i", "i'm", "im", "i've", "ive", "i'd", "i'll", "myself",
];

/// Another person's profile on this PC.
#[derive(Clone, Debug)]
pub(crate) struct Person {
    id: String,
    name: String,
    /// Linked to a person on the hub, so their layer syncs to their other PCs.
    linked: bool,
    /// Their person on the hub, when linked.
    hub_person: Option<String>,
}

impl Person {
    pub(crate) fn id(&self) -> &str {
        &self.id
    }

    pub(crate) fn name(&self) -> &str {
        &self.name
    }

    /// This person as one of their own chats would be known, for reading what applies to them.
    pub(crate) fn knowledge(&self) -> ChatKnowledge {
        ChatKnowledge {
            profile: Some(crate::launch::ProfileRef {
                id: self.id.clone(),
                name: self.name.clone(),
                is_home: false,
            }),
            project_id: None,
            ..ChatKnowledge::default()
        }
    }
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
    /// The trigger to write: `Some` replaces every trigger key (empty clears them), `None`
    /// leaves them alone. Absent in a card filed before triggers existed.
    #[serde(default)]
    when: Option<crate::rules::RuleWhen>,
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
struct MemoryChange {
    title: String,
    body: String,
    tags: Vec<String>,
    /// The hub person the memory becomes the property of.
    for_person: String,
    /// The hub person the asking chat acts as when it writes.
    asker_person: String,
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
    /// Absent in a card filed before memories could be saved for someone else.
    #[serde(default)]
    memory: Option<MemoryChange>,
}

/// Whether `text` uses a first-person word, so that saved under another person's name it would be
/// taken for their own words.
fn speaks_as_the_asker(text: &str) -> bool {
    text.split(|c: char| !(c.is_alphanumeric() || c == '\'' || c == '\u{2019}'))
        .any(|w| FIRST_PERSON.contains(&w.replace('\u{2019}', "'").to_lowercase().as_str()))
}

// ---- saving -------------------------------------------------------------------------------

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
                let hub_person = r.get::<_, Option<String>>(2)?.filter(|p| !p.is_empty());
                Ok::<_, rusqlite::Error>(Person {
                    id: r.get(0)?,
                    name: r.get(1)?,
                    linked: hub_person.is_some(),
                    hub_person,
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

/// Text for another person's rule or skill: line breaks and tabs pass, every other control
/// character and every invisible or direction-changing one is refused.
fn hidden_in(s: &str) -> bool {
    s.chars()
        .any(|c| c.is_control() && !matches!(c, '\n' | '\r' | '\t'))
        || helm_core::hidden::find_hidden(&Value::String(s.to_owned())).is_some()
}

fn asked_by(who: &ChatKnowledge) -> String {
    who.profile
        .as_ref()
        .map(|p| p.name.clone())
        .unwrap_or_else(|| "a chat with no person".into())
}

/// Save a rule into `person`'s layer, at once.
pub(crate) fn save_rule(
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
    if title.is_empty() || title.chars().count() > rules::MAX_TITLE_CHARS {
        return Err(ToolError::refused("A rule's title is 1 to 120 characters."));
    }
    if helm_core::hidden::has_hidden(&title) {
        return Err(ToolError::refused(
            "The rule cannot hold control, invisible or direction-changing characters.",
        ));
    }
    let data = tools::profile_dir(ctx)?;
    let root = Layer::Profile(person.id.clone()).root(&data);
    // A rule being replaced is read whole at stage time: its revision guards the apply, and a
    // staged change that leaves `text` out keeps the current text (shown on the card).
    let replace = match a.get("replace").and_then(Value::as_str) {
        Some(path) => {
            let (entry, current) = rules::read(&root, path).map_err(|_| {
                ToolError::refused(format!("{} has no rule {path} to replace.", person.name))
            })?;
            Some((
                Replaced {
                    path: entry.path,
                    title: entry.title,
                    revision: entry.revision,
                },
                rules::body_of(&current),
            ))
        }
        None => None,
    };
    let text = match (a.get("text").and_then(Value::as_str), &replace) {
        (Some(t), _) => t.trim().to_owned(),
        (None, Some((_, current))) => current.clone(),
        (None, None) => {
            return Err(ToolError::refused(
                "text is required unless replace names a rule.",
            ))
        }
    };
    if hidden_in(&text) {
        return Err(ToolError::refused(
            "The rule cannot hold control, invisible or direction-changing characters.",
        ));
    }
    if text.is_empty() || text.len() > rules::MAX_AGENT_RULE_TEXT {
        return Err(ToolError::refused(
            "A rule needs some text, and not too much.",
        ));
    }
    let when = match a.get("when") {
        Some(v) if v.is_object() => {
            let mut w: rules::RuleWhen = serde_json::from_value(v.clone()).map_err(|_| {
                ToolError::refused("when must hold trigger keys such as tools, commands or kinds.")
            })?;
            w.problems.clear();
            Some(w)
        }
        None | Some(Value::Null) => None,
        Some(_) => {
            return Err(ToolError::refused(
                "when must be an object of trigger keys.",
            ))
        }
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
            replace: replace.map(|(r, _)| r),
            when,
        }),
        skill: None,
        memory: None,
    };
    write(ctx, &staged)
}

/// Add a skill to `person`'s layer, at once.
pub(crate) fn save_skill(
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
        memory: None,
    };
    write(ctx, &staged)
}

/// The hub that memories are saved on: a test's stand-in, else the connected `helm_hub`.
fn hub_of(services: &helm_ipc::Services) -> Option<Arc<dyn MemoryHub>> {
    if let Some(h) = services.get::<MemoryHubHandle>() {
        return Some(h.0.clone());
    }
    let store = services.get::<helm_mcp::McpStore>()?;
    Some(Arc::new(HubMemory { store }))
}

fn hub_error(e: MemError) -> ToolError {
    match e {
        MemError::Unavailable(m) => ToolError::refused(format!(
            "[Helm] Nothing was saved or staged: shared memory cannot be reached ({m})"
        )),
        MemError::Refused(m) => {
            ToolError::refused(format!("[Helm] Nothing was saved or staged: {m}"))
        }
    }
}

const OLD_HUB: &str = "[Helm] Nothing was saved or staged: the hub is older than this Helm and cannot save a memory for another person. Update the hub first.";

/// Save a memory as `person`'s on the hub. The hub is asked only whether it can honour the save
/// (an old one cannot); nothing is written against one that cannot.
pub(crate) fn save_memory(
    ctx: &ToolCtx,
    who: &ChatKnowledge,
    person: Person,
    a: &Value,
) -> ToolResult {
    let text_of = |key: &str| {
        a.get(key)
            .and_then(Value::as_str)
            .map(str::trim)
            .filter(|t| !t.is_empty())
            .ok_or_else(|| ToolError::refused(format!("{key} is required.")))
    };
    let title = text_of("title")?.to_owned();
    let body = text_of("body")?.to_owned();
    if title.chars().count() > MAX_MEMORY_TITLE {
        return Err(ToolError::refused(
            "A memory's title is 1 to 120 characters.",
        ));
    }
    let tags: Vec<String> = match a.get("tags") {
        None | Some(Value::Null) => Vec::new(),
        Some(Value::Array(t)) => t
            .iter()
            .map(|t| t.as_str().map(|t| t.trim().to_owned()))
            .collect::<Option<_>>()
            .ok_or_else(|| ToolError::refused("tags is a list of words."))?,
        Some(_) => return Err(ToolError::refused("tags is a list of words.")),
    };
    if tags.len() > MAX_TAGS
        || tags
            .iter()
            .any(|t| t.is_empty() || t.chars().count() > MAX_TAG_CHARS)
    {
        return Err(ToolError::refused(
            "A memory takes up to 8 tags of up to 40 characters.",
        ));
    }
    if helm_core::hidden::has_hidden(&title)
        || hidden_in(&body)
        || tags.iter().any(|t| helm_core::hidden::has_hidden(t))
    {
        return Err(ToolError::refused(
            "The memory cannot hold control, invisible or direction-changing characters.",
        ));
    }
    let Some(for_person) = person.hub_person.clone() else {
        return Err(ToolError::refused(format!(
            "{} is not linked to the hub, and memories live there.",
            person.name
        )));
    };
    let Some(asker_person) =
        helm_mcp::lines::chat_person(&ctx.services, &ctx.caller_chat_id).person
    else {
        return Err(ToolError::refused(
            "This chat's person is not linked to the hub, so it cannot save a memory there.",
        ));
    };
    let staged = Staged {
        profile_id: person.id.clone(),
        profile_name: person.name.clone(),
        linked: person.linked,
        asked_by: asked_by(who),
        rule: None,
        skill: None,
        memory: Some(MemoryChange {
            title,
            body,
            tags,
            for_person,
            asker_person: asker_person.clone(),
        }),
    };
    let hub = hub_of(&ctx.services)
        .ok_or_else(|| ToolError::refused("Shared memory isn't connected."))?;
    if !hub
        .supports_save_for(Some(&asker_person))
        .map_err(hub_error)?
    {
        return Err(ToolError::refused(OLD_HUB));
    }
    write(ctx, &staged)
}

/// Make the change, and answer with what was done. The write is the one the layers always use,
/// and it is filed for Undo.
fn write(ctx: &ToolCtx, staged: &Staged) -> ToolResult {
    let outcome = match &staged.memory {
        Some(m) => apply_memory(
            &ctx.services,
            &ctx.caller_chat_id,
            &uuid::Uuid::new_v4().to_string(),
            staged,
            m,
        ),
        None => apply_staged(&ctx.services, &ctx.events, &ctx.caller_chat_id, staged),
    };
    match outcome.strip_prefix("Applied. ") {
        Some(done) => {
            let mut out = json!({"saved": true, "for": staged.profile_name, "note": done});
            if let Some(m) = &staged.memory {
                if speaks_as_the_asker(&m.body) {
                    out["warning"] = json!(format!(
                        "The text says my, me or I: it reads as {}'s own words.",
                        staged.profile_name
                    ));
                }
            }
            Ok(out)
        }
        None => Err(ToolError::refused(format!("[Helm] {outcome}"))),
    }
}

// ---- writing ------------------------------------------------------------------------------

/// Save a memory on the hub as the other person's. The write is keyed by `request_id`, so a retry
/// of the same call finds it already there.
fn apply_memory(
    services: &Arc<Services>,
    chat_id: &str,
    request_id: &str,
    s: &Staged,
    m: &MemoryChange,
) -> String {
    let nothing = |why: &str| format!("Nothing saved: {why}");
    // The profile must still be the one the card named, linked to the same person on the hub.
    let same = services.get::<Database>().is_some_and(|db| {
        db.snapshot(|tx| {
            tx.query_row(
                "SELECT name, hub_person_id FROM profiles WHERE id = ?1",
                [&s.profile_id],
                |r| Ok((r.get::<_, String>(0)?, r.get::<_, Option<String>>(1)?)),
            )
            .optional()
        })
        .ok()
        .flatten()
        .is_some_and(|(n, p)| n == s.profile_name && p.as_deref() == Some(m.for_person.as_str()))
    });
    if !same {
        return nothing(&format!(
            "{} is no longer a profile on this PC under that name and hub person. Ask again.",
            s.profile_name
        ));
    }
    let Some(hub) = hub_of(services) else {
        return nothing("shared memory isn't connected. Ask again.");
    };
    let args = json!({
        "title": m.title,
        "body": m.body,
        "tags": m.tags,
        "source": "helm",
        "name": format!("saved-for-{request_id}"),
        "for": m.for_person,
        "scope": "everyone",
    });
    match hub.call_approved(Some(&m.asker_person), "memory_write", args, &m.for_person) {
        Ok(saved) => {
            // The text is never logged: the chat, the profile and the hub's number.
            log::info!(
                "memory for another person: chat {chat_id} saved memory {} for profile {}",
                saved["id"],
                s.profile_id
            );
            format!(
                "Applied. Saved the memory {} for {} (asked by {}).",
                m.title, s.profile_name, s.asked_by
            )
        }
        Err(MemError::Refused(why)) if why.contains("already have a memory named") => {
            format!(
                "Applied. Already saved: the memory {} for {} was saved before.",
                m.title, s.profile_name
            )
        }
        Err(MemError::Unavailable(_)) => nothing("the hub could not be reached. Ask again."),
        Err(MemError::Refused(why)) => nothing(&why),
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
                r.when.as_ref(),
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

#[cfg(test)]
mod tests {
    use super::hidden_in;

    #[test]
    fn rule_text_refuses_control_and_invisible_characters_but_not_line_breaks_or_tabs() {
        for (text, refused) in [
            ("Always\nuse\r\ntabs\there", false),
            ("bell\u{7}", true),
            ("escape\u{1b}[31m", true),
            ("null\u{0}", true),
            ("direction\u{202e}", true),
            ("zero\u{200b}width", true),
        ] {
            assert_eq!(hidden_in(text), refused, "{text:?}");
        }
    }
}
