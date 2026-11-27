//! The ops behind Settings > Skills and the knowledge screens. They only ever touch the profile's
//! own knowledge folders; a picked source is read, never modified, and errors name what went
//! wrong in plain words.
//!
//! | op | class | args → result |
//! |---|---|---|
//! | `helm_skills_list` / `_add` / `_remove` / `_reveal` | blocking / maintenance | as before, each with an optional `layer` (default `everyone`) |
//! | `helm_agents_list` / `_add` / `_remove` / `_reveal` | blocking / maintenance | as before, with the same optional `layer` |
//! | `knowledge_get` | blocking | `{layer}` → `{layer, folder, rules, skills, agents, clashes, budget}` |
//! | `knowledge_rule_read` | blocking | `{layer, path}` → `{rule, text}` (the text without its title line) |
//! | `knowledge_rule_write` | maintenance | `{layer, path?, title, text, revision?, when?}` → `{rule}`; a stale `revision` is a conflict; `when` rewrites every trigger key (an empty one clears them) |
//! | `knowledge_rules_preview` | blocking | `{layer, folder?, kind?, person?}` (plus optional `tool`/`command`) → `{rules: [{path, layer, title, decision, fires, why}]}` — which rules a chat there would get |
//! | `knowledge_delete` | maintenance | `{layer, kind: rule\|skill\|agent, name}` → the layer as `knowledge_get` |
//! | `knowledge_move` | maintenance | `{kind, name, from, to}` → `{moved}`; refused when the name is taken there |
//! | `knowledge_copy` | maintenance | `{kind, name, from, to}` → `{copied}`; as a move, but the original stays |
//! | `knowledge_reveal` | blocking | `{layer, kind?}` → `{revealed}` |
//! | `knowledge_skill_read` | blocking | `{layer, name}` → `{text}` (the skill's `SKILL.md`; `file_read` refuses Helm's data folder) |
//! | `knowledge_memories_list` | blocking | `{layer}` → `{memories: [{id, title, owner, layer}]}` from the hub; code `unavailable` when it is not connected |
//! | `knowledge_memory_move` | maintenance | `{id, to}` (`to` a layer) → `{moved}`; the same `unavailable` |
//! | `skill_sources_get` | blocking | `{}` → `{sources, revision}` |
//! | `skill_sources_save` | maintenance | `{source, revision?}` → `{sources, revision}` |
//! | `skill_sources_remove` | maintenance | `{id, revision?}` → `{sources, revision}` |
//!
//! A layer is `everyone` or `profile:<profileId>`. Any profile on this PC may be named, so one
//! person can look after another's list; an id with no profile here is refused. Every change emits `knowledge-changed {layer}`
//! (and the older `skills-changed` or `agents-changed` for the older ops).

use crate::launch::{self, ChatKnowledge, ProfileRef};
use crate::layers::{Kind, Layer};
use crate::memories::{self, HubMemory, MemError};
use crate::sources::{self, SkillSource};
use crate::triggers;
use crate::undo::item_path;
use crate::{agents, examples, plugins, rules, SkillEntry};
use helm_core::profile::Profile;
use helm_ipc::{ApiError, AppEvent, OpClass, OpCtx, OpRegistry};
use rusqlite::OptionalExtension;
use serde::Deserialize;
use serde_json::{json, Value};
use std::io::{self, ErrorKind};
use std::path::{Path, PathBuf};

pub const EVENT: &str = "skills-changed";
pub const AGENTS_EVENT: &str = "agents-changed";
pub const KNOWLEDGE_EVENT: &str = "knowledge-changed";

#[derive(Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
struct AddArgs {
    source: String,
    #[serde(default)]
    layer: Option<String>,
}

#[derive(Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
struct RemoveArgs {
    name: String,
    #[serde(default)]
    layer: Option<String>,
}

#[derive(Deserialize, Default)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
struct RevealArgs {
    /// One skill's folder; the skills root itself when absent.
    #[serde(default)]
    name: Option<String>,
    #[serde(default)]
    layer: Option<String>,
}

#[derive(Deserialize, Default)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
struct LayerArgs {
    #[serde(default)]
    layer: Option<String>,
}

#[derive(Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
struct GetArgs {
    layer: String,
}

#[derive(Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
struct RuleReadArgs {
    layer: String,
    path: String,
}

#[derive(Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
struct RuleWriteArgs {
    layer: String,
    #[serde(default)]
    path: Option<String>,
    title: String,
    text: String,
    #[serde(default)]
    revision: Option<String>,
    /// Set or clear the header's "always include in full" flag; absent keeps it as it is.
    #[serde(default)]
    always: Option<bool>,
    /// Rewrite every trigger key to this: `Some` replaces them (an all-empty `RuleWhen` clears
    /// the trigger keys), `None` leaves them alone. Unknown header lines survive either way.
    #[serde(default)]
    when: Option<rules::RuleWhen>,
}

#[derive(Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
struct DeleteArgs {
    layer: String,
    kind: String,
    name: String,
}

#[derive(Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
struct MoveArgs {
    kind: String,
    name: String,
    from: String,
    to: String,
}

#[derive(Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
struct KnowledgeRevealArgs {
    layer: String,
    #[serde(default)]
    kind: Option<String>,
}

#[derive(Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
struct SourceSaveArgs {
    source: SkillSource,
    #[serde(default)]
    revision: Option<String>,
}

#[derive(Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
struct SourceRemoveArgs {
    id: String,
    #[serde(default)]
    revision: Option<String>,
}

fn profile(ctx: &OpCtx) -> Result<PathBuf, ApiError> {
    ctx.services
        .need::<Profile>()
        .map(|p| p.dir.clone())
        .map_err(|_| ApiError::internal("Helm has no profile folder."))
}

fn parse<T: serde::de::DeserializeOwned>(v: Value, what: &str) -> Result<T, ApiError> {
    serde_json::from_value(v).map_err(|_| ApiError::bad_args(format!("Expected {what}.")))
}

fn layer_of(raw: Option<&str>) -> Result<Layer, ApiError> {
    match raw.map(str::trim) {
        None | Some("") => Ok(Layer::Everyone),
        Some(l) => Layer::parse(l)
            .ok_or_else(|| ApiError::bad_args("The layer is `everyone` or `profile:<id>`.")),
    }
}

fn kind_of(raw: &str) -> Result<Kind, ApiError> {
    Kind::parse(raw).ok_or_else(|| ApiError::bad_args("The kind is `rule`, `skill` or `agent`."))
}

/// A person's layer must belong to a profile on this PC: a made-up or removed id is refused, and
/// no folder is made for it.
fn layer_exists(ctx: &OpCtx, layer: &Layer) -> Result<(), ApiError> {
    let Layer::Profile(id) = layer else {
        return Ok(());
    };
    let db = ctx
        .services
        .need::<helm_core::db::Database>()
        .map_err(|_| ApiError::internal("Helm has no database."))?;
    let found = db
        .snapshot(|tx| {
            tx.query_row("SELECT 1 FROM profiles WHERE id = ?1", [id], |_| Ok(()))
                .optional()
        })
        .map_err(|_| ApiError::internal("The profiles could not be read."))?;
    found
        .map(|_| ())
        .ok_or_else(|| ApiError::bad_args("No profile on this PC has that list."))
}

/// The profile folder, the layer an op names and its root; the examples are offered first.
fn layer_root(ctx: &OpCtx, raw: Option<&str>) -> Result<(PathBuf, Layer, PathBuf), ApiError> {
    let data = profile(ctx)?;
    let layer = layer_of(raw)?;
    layer_exists(ctx, &layer)?;
    examples::seed(&data);
    let root = layer.root(&data);
    Ok((data, layer, root))
}

fn knowledge_changed(ctx: &OpCtx, layer: &Layer) {
    ctx.events
        .emit(AppEvent::new(KNOWLEDGE_EVENT).with("layer", layer.key()));
}

fn changed(ctx: &OpCtx, layer: &Layer) {
    ctx.events.emit(AppEvent::new(EVENT));
    knowledge_changed(ctx, layer);
}

/// An I/O refusal as the caller sees it: the sentence for a bad request, an internal error for the
/// rest.
fn api_error(e: io::Error, failed: &str) -> ApiError {
    match e.kind() {
        ErrorKind::NotFound | ErrorKind::InvalidInput | ErrorKind::AlreadyExists => {
            ApiError::bad_args(e.to_string())
        }
        _ => ApiError::internal(failed),
    }
}

/// As [`api_error`], with `AlreadyExists` meaning the thing changed since it was read.
fn conflict_error(e: io::Error, failed: &str) -> ApiError {
    match e.kind() {
        ErrorKind::AlreadyExists => ApiError::conflict(e.to_string()),
        _ => api_error(e, failed),
    }
}

fn to_entry_value(list: Vec<SkillEntry>) -> Value {
    json!({ "skills": serde_json::to_value(list).unwrap_or(Value::Null) })
}

fn list(ctx: &OpCtx, args: Value) -> Result<Value, ApiError> {
    let a: LayerArgs = parse(args, "an optional layer")?;
    let (_, _, root) = layer_root(ctx, a.layer.as_deref())?;
    let skills = crate::list(&root);
    Ok(json!({
        "skills": skills,
        "folder": crate::skills_dir(&root).to_string_lossy(),
        "loadedIntoChats": !skills.is_empty(),
    }))
}

fn add(ctx: &OpCtx, v: Value) -> Result<Value, ApiError> {
    let a: AddArgs = parse(v, "a source folder path")?;
    let source = a.source.trim().to_owned();
    if source.is_empty() {
        return Err(ApiError::bad_args("Pick a skill folder first."));
    }
    let (_, layer, root) = layer_root(ctx, a.layer.as_deref())?;
    let added = crate::add(&root, Path::new(&source)).map_err(|e| match e.kind() {
        ErrorKind::NotFound => ApiError::bad_args(format!("That folder cannot be used: {e}")),
        _ => api_error(e, "The skill folder could not be copied."),
    })?;
    changed(ctx, &layer);
    let mut body = to_entry_value(crate::list(&root));
    body.as_object_mut()
        .unwrap()
        .insert("added".into(), json!(added.name));
    Ok(body)
}

fn remove(ctx: &OpCtx, v: Value) -> Result<Value, ApiError> {
    let a: RemoveArgs = parse(v, "a skill name")?;
    let (_, layer, root) = layer_root(ctx, a.layer.as_deref())?;
    crate::remove(&root, &a.name).map_err(|e| match e.kind() {
        ErrorKind::NotFound => ApiError::bad_args("No skill of that name is in the folder."),
        ErrorKind::InvalidInput => ApiError::bad_args("That is not a skill name."),
        _ => ApiError::internal("The skill could not be removed."),
    })?;
    changed(ctx, &layer);
    Ok(to_entry_value(crate::list(&root)))
}

fn reveal(ctx: &OpCtx, v: Value) -> Result<Value, ApiError> {
    let a: RevealArgs = parse(v, "an optional skill name")?;
    let (_, _, root) = layer_root(ctx, a.layer.as_deref())?;
    let path = match a.name {
        Some(n) if !n.trim().is_empty() => {
            if !crate::is_valid_name(n.trim()) {
                return Err(ApiError::bad_args("That is not a skill name."));
            }
            crate::skills_dir(&root).join(n.trim())
        }
        _ => crate::skills_dir(&root),
    };
    if !path.is_dir() {
        return Err(ApiError::bad_args("That folder is not here."));
    }
    reveal_folder(&path).map_err(|_| ApiError::internal("Explorer could not be started."))?;
    Ok(json!({"revealed": true}))
}

fn agents_changed(ctx: &OpCtx, layer: &Layer) {
    ctx.events.emit(AppEvent::new(AGENTS_EVENT));
    knowledge_changed(ctx, layer);
}

fn agents_list(ctx: &OpCtx, args: Value) -> Result<Value, ApiError> {
    let a: LayerArgs = parse(args, "an optional layer")?;
    let (_, _, root) = layer_root(ctx, a.layer.as_deref())?;
    Ok(json!({
        "agents": agents::list(&root),
        "folder": agents::agents_dir(&root).to_string_lossy(),
    }))
}

fn agents_add(ctx: &OpCtx, v: Value) -> Result<Value, ApiError> {
    let a: AddArgs = parse(v, "a source file path")?;
    let source = a.source.trim().to_owned();
    if source.is_empty() {
        return Err(ApiError::bad_args("Pick an agent file first."));
    }
    let (_, layer, root) = layer_root(ctx, a.layer.as_deref())?;
    let added = agents::add(&root, Path::new(&source))
        .map_err(|e| api_error(e, "The agent file could not be copied."))?;
    agents_changed(ctx, &layer);
    Ok(json!({ "agents": agents::list(&root), "added": added.names, "skipped": added.skipped }))
}

fn agents_remove(ctx: &OpCtx, v: Value) -> Result<Value, ApiError> {
    let a: RemoveArgs = parse(v, "an agent name")?;
    let (_, layer, root) = layer_root(ctx, a.layer.as_deref())?;
    agents::remove(&root, &a.name).map_err(|e| match e.kind() {
        ErrorKind::NotFound => ApiError::bad_args("No agent of that name is in the folder."),
        ErrorKind::InvalidInput => ApiError::bad_args("That is not an agent name."),
        _ => ApiError::internal("The agent could not be removed."),
    })?;
    agents_changed(ctx, &layer);
    Ok(json!({ "agents": agents::list(&root) }))
}

fn agents_reveal(ctx: &OpCtx, args: Value) -> Result<Value, ApiError> {
    let a: LayerArgs = parse(args, "an optional layer")?;
    let (_, _, root) = layer_root(ctx, a.layer.as_deref())?;
    let path = agents::agents_dir(&root);
    if !path.is_dir() {
        return Err(ApiError::bad_args("That folder is not here."));
    }
    reveal_folder(&path).map_err(|_| ApiError::internal("Explorer could not be started."))?;
    Ok(json!({"revealed": true}))
}

/// What a chat reading this layer spends of the whole-rule budget. A person's layer comes with
/// Everyone's rules as a chat gets them; the Everyone layer reads alone. The name and home flag
/// never change the figure, so they are left blank.
fn budget_view(data: &Path, layer: &Layer) -> Value {
    let who = match layer {
        Layer::Everyone => ChatKnowledge::default(),
        Layer::Profile(id) => ChatKnowledge {
            profile: Some(ProfileRef {
                id: id.clone(),
                name: String::new(),
                is_home: false,
            }),
            project_id: None,
            ..ChatKnowledge::default()
        },
    };
    let (used, cap) = launch::rules_budget(data, &who);
    json!({ "usedBytes": used, "capBytes": cap })
}

/// One layer as the knowledge screens draw it.
fn view(data: &Path, layer: &Layer) -> Value {
    let root = layer.root(data);
    let (skills, clashes) = plugins::layer_view(data, layer);
    json!({
        "layer": layer.key(),
        "folder": root.to_string_lossy(),
        "rules": rules::list(&root),
        "skills": skills,
        "agents": agents::list(&root),
        "clashes": clashes,
        "budget": budget_view(data, layer),
    })
}

fn knowledge_get(ctx: &OpCtx, v: Value) -> Result<Value, ApiError> {
    let a: GetArgs = parse(v, "a layer")?;
    let (data, layer, _) = layer_root(ctx, Some(&a.layer))?;
    Ok(view(&data, &layer))
}

fn knowledge_rule_read(ctx: &OpCtx, v: Value) -> Result<Value, ApiError> {
    let a: RuleReadArgs = parse(v, "a layer and a rule path")?;
    let (_, _, root) = layer_root(ctx, Some(&a.layer))?;
    let (rule, text) =
        rules::read(&root, &a.path).map_err(|e| api_error(e, "The rule could not be read."))?;
    Ok(json!({ "rule": rule, "text": rules::body_of(&text) }))
}

fn knowledge_rule_write(ctx: &OpCtx, v: Value) -> Result<Value, ApiError> {
    let a: RuleWriteArgs = parse(v, "a layer, a title and the rule's text")?;
    let (_, layer, root) = layer_root(ctx, Some(&a.layer))?;
    let mut when = a.when;
    if let Some(w) = when.as_mut() {
        // `problems` is read-only: what the old header could not say is not carried forward.
        w.problems.clear();
    }
    let rule = rules::write(
        &root,
        a.path.as_deref(),
        &a.title,
        &a.text,
        a.revision.as_deref(),
        a.always,
        when.as_ref(),
        rules::MAX_RULE_FILE as usize,
    )
    .map_err(|e| conflict_error(e, "The rule could not be saved."))?;
    knowledge_changed(ctx, &layer);
    Ok(json!({ "rule": rule }))
}

#[derive(Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
struct RulesPreviewArgs {
    layer: String,
    /// The folder the imagined chat runs in.
    #[serde(default)]
    folder: Option<String>,
    /// `coding`, `orchestrating` or `household`; derived from the folder when absent.
    #[serde(default)]
    kind: Option<String>,
    /// The profile name of the imagined chat's person.
    #[serde(default)]
    person: Option<String>,
    /// One tool call to test moment triggers against.
    #[serde(default)]
    tool: Option<String>,
    #[serde(default)]
    command: Option<String>,
}

/// Explain one rule's preview result in a short line the panel can show under it.
fn preview_why(w: &rules::RuleWhen, facts: &triggers::Facts) -> String {
    if w.is_empty() {
        return "no When: every chat gets it".to_owned();
    }
    let lower = |list: &[String]| list.join(", ");
    if !w.kinds.is_empty()
        && !w
            .kinds
            .iter()
            .any(|k| k.eq_ignore_ascii_case(facts.kind.as_str()))
    {
        return format!(
            "kinds says {} — this chat is {}",
            lower(&w.kinds),
            facts.kind.as_str()
        );
    }
    if !w.people.is_empty()
        && !w.people.iter().any(|p| {
            facts
                .person
                .as_deref()
                .is_some_and(|n| n.eq_ignore_ascii_case(p))
        })
    {
        return format!("people says {}", lower(&w.people));
    }
    if !w.pcs.is_empty() && !w.pcs.iter().any(|p| p.eq_ignore_ascii_case(&facts.pc)) {
        return format!("pcs says {} — this PC is {}", lower(&w.pcs), facts.pc);
    }
    let today = facts.today.to_string();
    if w.from.as_deref().is_some_and(|f| f > today.as_str()) {
        return format!("starts on {}", w.from.as_deref().unwrap_or(""));
    }
    if w.until.as_deref().is_some_and(|u| u < today.as_str()) {
        return format!("ended on {}", w.until.as_deref().unwrap_or(""));
    }
    if w.has_moment() {
        let mut bits: Vec<String> = Vec::new();
        for (key, list) in [
            ("folders", &w.folders),
            ("hosts", &w.hosts),
            ("tools", &w.tools),
            ("commands", &w.commands),
        ] {
            if !list.is_empty() {
                bits.push(format!("{key}: {}", lower(list)));
            }
        }
        return format!("waits for one of {}", bits.join(" · "));
    }
    "its When fits this chat".to_owned()
}

/// `knowledge_rules_preview`: which rules a chat with the given folder, kind and person would be
/// shown at launch — and whether the named tool call would fire a moment rule. Like the budget
/// view, a person's layer is read together with Everyone's, as a chat gets them.
fn knowledge_rules_preview(ctx: &OpCtx, v: Value) -> Result<Value, ApiError> {
    let a: RulesPreviewArgs = parse(v, "a layer and what to imagine about the chat")?;
    let (data, layer, _) = layer_root(ctx, Some(&a.layer))?;
    let person_name = match (&layer, &a.person) {
        (_, Some(p)) => p.clone(),
        (Layer::Everyone, None) => String::new(),
        // The imagined chat belongs to that list's person, so its person is that profile's NAME:
        // a `people:` rule compares names, and the raw id would never match.
        (Layer::Profile(id), None) => ctx
            .services
            .get::<helm_core::db::Database>()
            .and_then(|db| {
                db.snapshot(|tx| {
                    tx.query_row("SELECT name FROM profiles WHERE id = ?1", [id], |r| {
                        r.get::<_, String>(0)
                    })
                    .optional()
                })
                .ok()
                .flatten()
            })
            .unwrap_or_default(),
    };
    let who = ChatKnowledge {
        profile: Some(ProfileRef {
            id: match &layer {
                Layer::Profile(id) => id.clone(),
                Layer::Everyone => String::new(),
            },
            name: person_name,
            is_home: false,
        }),
        project_id: None,
        cwd: a.folder.clone().filter(|f| !f.trim().is_empty()),
        orchestrating: a.kind.as_deref() == Some("orchestrating"),
        household: a.kind.as_deref() == Some("household"),
    };
    let mut facts = triggers::Facts::of(&who, None);
    if let Some(k) = a.kind.as_deref().and_then(triggers::ChatKind::parse) {
        facts.kind = k;
    }
    let moment = a.tool.as_deref().map(|t| {
        let input = match &a.command {
            Some(c) => json!({"command": c}),
            None => json!({}),
        };
        triggers::Moment::from_call(t, &input, facts.cwd.as_deref())
    });
    let mut rules_out: Vec<Value> = Vec::new();
    for (group, items) in launch::rule_groups(&data, &who) {
        let layer_key = if group == "Rules for everyone" {
            "everyone".to_owned()
        } else {
            layer.key()
        };
        for r in items {
            let decision = triggers::at_launch(
                &r.front.when,
                r.front.always,
                r.body.chars().count(),
                &facts,
            );
            let fires = moment
                .as_ref()
                .is_some_and(|m| triggers::fires(&r.front.when, &facts, m));
            rules_out.push(json!({
                "path": r.entry.path,
                "layer": layer_key,
                "title": r.entry.title,
                "decision": match decision {
                    triggers::Decision::Full => "full",
                    triggers::Decision::Line => "line",
                    triggers::Decision::Hidden => "hidden",
                },
                "fires": fires,
                "why": preview_why(&r.front.when, &facts),
            }));
        }
    }
    Ok(json!({ "rules": rules_out }))
}

fn knowledge_delete(ctx: &OpCtx, v: Value) -> Result<Value, ApiError> {
    let a: DeleteArgs = parse(v, "a layer, a kind and a name")?;
    let kind = kind_of(&a.kind)?;
    let (data, layer, root) = layer_root(ctx, Some(&a.layer))?;
    let done = match kind {
        Kind::Rule => rules::remove(&root, &a.name),
        Kind::Skill => crate::remove(&root, &a.name),
        Kind::Agent => agents::remove(&root, &a.name),
    };
    done.map_err(|e| api_error(e, "It could not be removed."))?;
    knowledge_changed(ctx, &layer);
    Ok(view(&data, &layer))
}

/// Copy one item to another layer, then remove it from the first. Nothing already in the second
/// layer is overwritten.
fn knowledge_move(ctx: &OpCtx, v: Value) -> Result<Value, ApiError> {
    transfer(ctx, v, true).map(|name| json!({ "moved": name }))
}

/// Copy one item to another layer and leave the original where it is: how one person's rule
/// becomes a variant in someone else's list. Nothing already in the second layer is overwritten.
fn knowledge_copy(ctx: &OpCtx, v: Value) -> Result<Value, ApiError> {
    transfer(ctx, v, false).map(|name| json!({ "copied": name }))
}

fn transfer(ctx: &OpCtx, v: Value, remove_original: bool) -> Result<String, ApiError> {
    let a: MoveArgs = parse(v, "a kind, a name and two layers")?;
    let kind = kind_of(&a.kind)?;
    let data = profile(ctx)?;
    let (from, to) = (layer_of(Some(&a.from))?, layer_of(Some(&a.to))?);
    if from == to {
        return Err(ApiError::bad_args("Pick a different layer to move it to."));
    }
    layer_exists(ctx, &from)?;
    layer_exists(ctx, &to)?;
    let src = item_path(&data, &from, kind, &a.name)
        .map_err(|e| api_error(e, "That item could not be found."))?;
    let dest = item_path(&data, &to, kind, &a.name)
        .map_err(|e| api_error(e, "That item could not be moved."))?;
    if !src.exists() {
        return Err(ApiError::bad_args("There is nothing of that name to move."));
    }
    if dest.exists() {
        return Err(ApiError::conflict(
            "The other layer already has one of that name.",
        ));
    }
    let copied = if kind == Kind::Skill {
        crate::install_dir(&src, &dest)
    } else {
        let cap = rules::MAX_RULE_FILE.max(agents::MAX_AGENT_FILE);
        helm_core::privfs::read_capped(&src, cap).and_then(|bytes| {
            if let Some(parent) = dest.parent() {
                helm_core::privfs::create_private_dir_all(parent)?;
            }
            helm_core::privfs::write_atomic(&dest, &bytes)
        })
    };
    copied.map_err(|e| api_error(e, "The item could not be copied."))?;
    if remove_original {
        let removed = if kind == Kind::Skill {
            std::fs::remove_dir_all(&src)
        } else {
            std::fs::remove_file(&src)
        };
        removed.map_err(|_| {
            ApiError::internal("It was copied, but the original could not be removed.")
        })?;
        knowledge_changed(ctx, &from);
    }
    // A rule that just landed in (or left) a layer must reach the rule index at once: copy and
    // move write the file directly, without going through `rules::write`.
    if kind == Kind::Rule {
        crate::launch::forget_rules(&from.root(&data));
        crate::launch::forget_rules(&to.root(&data));
    }
    knowledge_changed(ctx, &to);
    Ok(a.name)
}

fn knowledge_reveal(ctx: &OpCtx, v: Value) -> Result<Value, ApiError> {
    let a: KnowledgeRevealArgs = parse(v, "a layer and an optional kind")?;
    let (_, _, root) = layer_root(ctx, Some(&a.layer))?;
    let path = match a.kind.as_deref() {
        None | Some("") => root,
        Some(k) => root.join(kind_of(k)?.folder()),
    };
    helm_core::privfs::create_private_dir_all(&path)
        .map_err(|_| ApiError::internal("The folder could not be made."))?;
    reveal_folder(&path).map_err(|_| ApiError::internal("Explorer could not be started."))?;
    Ok(json!({"revealed": true}))
}

fn sources_error(e: io::Error) -> ApiError {
    match e.kind() {
        ErrorKind::AlreadyExists => ApiError::conflict(e.to_string()),
        ErrorKind::NotFound | ErrorKind::InvalidInput => ApiError::bad_args(e.to_string()),
        ErrorKind::InvalidData => ApiError::internal(e.to_string()),
        _ => ApiError::internal("The linked folders could not be saved."),
    }
}

fn skill_sources_get(ctx: &OpCtx, _args: Value) -> Result<Value, ApiError> {
    let data = profile(ctx)?;
    let snap = sources::load(&data).map_err(sources_error)?;
    Ok(serde_json::to_value(snap).unwrap_or(Value::Null))
}

fn skill_sources_save(ctx: &OpCtx, v: Value) -> Result<Value, ApiError> {
    let a: SourceSaveArgs = parse(v, "a linked folder and the revision it was based on")?;
    let data = profile(ctx)?;
    let layer = layer_of(Some(&a.source.layer)).ok();
    let snap = sources::save(&data, a.source, a.revision.as_deref()).map_err(sources_error)?;
    knowledge_changed(ctx, &layer.unwrap_or(Layer::Everyone));
    Ok(serde_json::to_value(snap).unwrap_or(Value::Null))
}

fn skill_sources_remove(ctx: &OpCtx, v: Value) -> Result<Value, ApiError> {
    let a: SourceRemoveArgs = parse(v, "a linked folder id")?;
    let data = profile(ctx)?;
    let snap = sources::remove(&data, &a.id, a.revision.as_deref()).map_err(sources_error)?;
    knowledge_changed(ctx, &Layer::Everyone);
    Ok(serde_json::to_value(snap).unwrap_or(Value::Null))
}

/// Open a folder in Explorer (Windows) or the file browser elsewhere.
#[cfg(windows)]
fn reveal_folder(path: &Path) -> std::io::Result<()> {
    use std::os::windows::process::CommandExt;
    const CREATE_NO_WINDOW: u32 = 0x0800_0000;
    let windir = std::env::var_os("SystemRoot").unwrap_or_else(|| "C:\\Windows".into());
    let mut cmd = std::process::Command::new(std::path::PathBuf::from(windir).join("explorer.exe"));
    cmd.arg(path).creation_flags(CREATE_NO_WINDOW);
    cmd.spawn()?;
    Ok(())
}

#[cfg(not(windows))]
fn reveal_folder(_path: &Path) -> std::io::Result<()> {
    Err(std::io::Error::other("not supported here"))
}

#[derive(Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
struct SkillReadArgs {
    layer: String,
    name: String,
}

fn knowledge_skill_read(ctx: &OpCtx, v: Value) -> Result<Value, ApiError> {
    let a: SkillReadArgs = parse(v, "a layer and a skill name")?;
    if !crate::is_valid_name(&a.name) {
        return Err(ApiError::bad_args("That is not a skill name."));
    }
    let (_, _, root) = layer_root(ctx, Some(&a.layer))?;
    let path = crate::skills_dir(&root).join(&a.name).join("SKILL.md");
    let bytes = helm_core::privfs::read_capped(&path, crate::MAX_SKILL_FILE)
        .map_err(|e| api_error(e, "The skill could not be read."))?;
    Ok(json!({ "text": String::from_utf8_lossy(&bytes) }))
}

#[derive(Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
struct MemoriesListArgs {
    layer: String,
}

#[derive(Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
struct MemoryMoveArgs {
    /// The memory's number, as text or as a number.
    id: Value,
    to: String,
}

fn memory_hub(ctx: &OpCtx) -> Result<HubMemory, ApiError> {
    let store = ctx
        .services
        .get::<helm_mcp::McpStore>()
        .ok_or_else(|| ApiError::new("unavailable", "Shared memory isn't connected."))?;
    Ok(HubMemory { store })
}

fn memory_error(e: MemError) -> ApiError {
    match e {
        MemError::Unavailable(m) => ApiError::new("unavailable", m),
        MemError::Refused(m) => ApiError::bad_args(m),
    }
}

/// The hub person a layer's memories are read as, and every linked person (this Helm's own first).
fn memory_people(ctx: &OpCtx, layer: &Layer) -> Result<(Option<String>, Vec<String>), ApiError> {
    let db = ctx
        .services
        .need::<helm_core::db::Database>()
        .map_err(|_| ApiError::internal("Helm has no database."))?;
    // (profile id, hub person, is home); read here because the profiles crate sits above the
    // chats crate that loads this one.
    let mut all: Vec<(String, Option<String>, bool)> = db
        .snapshot(|tx| {
            let mut st = tx.prepare("SELECT id, hub_person_id, is_home FROM profiles")?;
            let rows = st
                .query_map([], |r| Ok((r.get(0)?, r.get(1)?, r.get(2)?)))?
                .collect::<rusqlite::Result<Vec<_>>>()?;
            Ok::<_, rusqlite::Error>(rows)
        })
        .map_err(|_| ApiError::internal("The profiles could not be read."))?;
    all.sort_by_key(|p| !p.2);
    let person = |p: &(String, Option<String>, bool)| p.1.clone().filter(|x| !x.is_empty());
    let of_layer = match layer {
        Layer::Everyone => all.iter().find(|p| p.2).and_then(person),
        Layer::Profile(id) => all.iter().find(|p| &p.0 == id).and_then(person),
    };
    let mut linked: Vec<String> = Vec::new();
    for p in all.iter().filter_map(person) {
        if !linked.contains(&p) {
            linked.push(p);
        }
    }
    Ok((of_layer, linked))
}

fn knowledge_memories_list(ctx: &OpCtx, v: Value) -> Result<Value, ApiError> {
    let a: MemoriesListArgs = parse(v, "a layer")?;
    let layer = layer_of(Some(&a.layer))?;
    let hub = memory_hub(ctx)?;
    let (person, _) = memory_people(ctx, &layer)?;
    let rows = memories::list(&hub, &layer, person.as_deref()).map_err(memory_error)?;
    Ok(json!({ "memories": rows }))
}

fn knowledge_memory_move(ctx: &OpCtx, v: Value) -> Result<Value, ApiError> {
    let a: MemoryMoveArgs = parse(v, "a memory id and a layer to move it to")?;
    let id = match &a.id {
        Value::Number(n) => n.as_i64(),
        Value::String(t) => t.trim().parse().ok(),
        _ => None,
    }
    .ok_or_else(|| ApiError::bad_args("Expected a memory id."))?;
    let to = layer_of(Some(&a.to))?;
    let hub = memory_hub(ctx)?;
    let (to_person, linked) = memory_people(ctx, &to)?;
    let (home, _) = memory_people(ctx, &Layer::Everyone)?;
    memories::move_to(
        &hub,
        id,
        &to,
        to_person.as_deref(),
        home.as_deref(),
        &linked,
    )
    .map_err(memory_error)?;
    Ok(json!({ "moved": id.to_string() }))
}

pub fn register_ops(reg: &mut OpRegistry) {
    reg.blocking("helm_skills_list", OpClass::Blocking, list)
        .blocking("helm_skills_add", OpClass::Maintenance, add)
        .blocking("helm_skills_remove", OpClass::Maintenance, remove)
        .blocking("helm_skills_reveal", OpClass::Blocking, reveal)
        .blocking("helm_agents_list", OpClass::Blocking, agents_list)
        .blocking("helm_agents_add", OpClass::Maintenance, agents_add)
        .blocking("helm_agents_remove", OpClass::Maintenance, agents_remove)
        .blocking("helm_agents_reveal", OpClass::Blocking, agents_reveal)
        .blocking("knowledge_get", OpClass::Blocking, knowledge_get)
        .blocking(
            "knowledge_rule_read",
            OpClass::Blocking,
            knowledge_rule_read,
        )
        .blocking(
            "knowledge_rule_write",
            OpClass::Maintenance,
            knowledge_rule_write,
        )
        .blocking(
            "knowledge_rules_preview",
            OpClass::Blocking,
            knowledge_rules_preview,
        )
        .blocking("knowledge_delete", OpClass::Maintenance, knowledge_delete)
        .blocking("knowledge_move", OpClass::Maintenance, knowledge_move)
        .blocking("knowledge_copy", OpClass::Maintenance, knowledge_copy)
        .blocking("knowledge_reveal", OpClass::Blocking, knowledge_reveal)
        .blocking(
            "knowledge_skill_read",
            OpClass::Blocking,
            knowledge_skill_read,
        )
        .blocking(
            "knowledge_memories_list",
            OpClass::Blocking,
            knowledge_memories_list,
        )
        .blocking(
            "knowledge_memory_move",
            OpClass::Maintenance,
            knowledge_memory_move,
        )
        .blocking("skill_sources_get", OpClass::Blocking, skill_sources_get)
        .blocking(
            "skill_sources_save",
            OpClass::Maintenance,
            skill_sources_save,
        )
        .blocking(
            "skill_sources_remove",
            OpClass::Maintenance,
            skill_sources_remove,
        );
}

#[cfg(test)]
mod tests {
    use super::*;
    use helm_core::db::{Database, MigrationRegistry};
    use std::sync::Arc;

    fn profiles_table(tx: &rusqlite::Transaction<'_>) -> rusqlite::Result<()> {
        tx.execute_batch(
            "CREATE TABLE profiles (id TEXT PRIMARY KEY, name TEXT NOT NULL, hub_person_id TEXT,
                                    is_home INTEGER NOT NULL DEFAULT 0);
             INSERT INTO profiles(id, name, is_home) VALUES ('alex', 'Alex', 1), ('sam', 'Sam', 0);",
        )
    }

    /// One person can see and look after another's list: every op takes any profile on this PC,
    /// a copy leaves the original, and a profile that is not here is refused with nothing made.
    #[test]
    fn another_persons_list_can_be_read_edited_and_copied_to() {
        let tmp = tempfile::tempdir().unwrap();
        let data = tmp.path().join("data");
        std::fs::create_dir_all(&data).unwrap();
        let mut dbreg = MigrationRegistry::new();
        dbreg.initializer("profiles-test", profiles_table);
        let db: Arc<Database> = Arc::new(Database::open(&data.join("t.db"), &dbreg).unwrap());
        let mut services = helm_ipc::Services::new();
        services.insert(db);
        services.insert(Arc::new(Profile {
            dir: data.clone(),
            hash: String::new(),
        }));
        let ctx = OpCtx {
            services: Arc::new(services),
            events: Arc::new(helm_ipc::NullSink),
        };
        let alex = "profile:alex";
        let sam = "profile:sam";
        let titles = |layer: &str| -> Vec<String> {
            let v = knowledge_get(&ctx, json!({ "layer": layer })).unwrap();
            v["rules"]
                .as_array()
                .unwrap()
                .iter()
                .map(|r| r["path"].as_str().unwrap().to_owned())
                .collect()
        };

        // Write into Sam's list (a new rule), read it back, then edit it at the revision read.
        let made = knowledge_rule_write(
            &ctx,
            json!({"layer": sam, "title": "Quiet hours", "text": "No loud tools after ten."}),
        )
        .unwrap();
        let path = made["rule"]["path"].as_str().unwrap().to_owned();
        assert_eq!(path, "rules/quiet-hours.md");
        assert_eq!(titles(sam), [path.as_str()]);
        assert!(titles(alex).is_empty(), "Alex's own list is untouched");
        let read = knowledge_rule_read(&ctx, json!({"layer": sam, "path": path})).unwrap();
        assert_eq!(read["text"], "No loud tools after ten.");
        let revision = read["rule"]["revision"].as_str().unwrap().to_owned();
        knowledge_rule_write(
            &ctx,
            json!({"layer": sam, "path": path, "title": "Quiet hours", "text": "Nothing loud.", "revision": revision}),
        )
        .unwrap();
        let stale = knowledge_rule_write(
            &ctx,
            json!({"layer": sam, "path": path, "title": "Quiet hours", "text": "Again.", "revision": revision}),
        )
        .unwrap_err();
        assert_eq!(stale.code, "conflict");

        // A write carrying `when`: the trigger round-trips on the entry and feeds a summary.
        let trig = knowledge_rule_write(
            &ctx,
            json!({
                "layer": sam,
                "title": "Push gates",
                "text": "Ask before pushing.",
                "when": {"commands": ["git push"], "kinds": ["coding"]}
            }),
        )
        .unwrap();
        let tpath = trig["rule"]["path"].as_str().unwrap().to_owned();
        assert_eq!(tpath, "rules/push-gates.md");
        assert_eq!(trig["rule"]["when"]["commands"][0], "git push");
        assert_eq!(trig["rule"]["when"]["kinds"][0], "coding");
        assert!(trig["rule"]["whenSummary"]
            .as_str()
            .unwrap_or_default()
            .contains("git push"));

        // The preview answers as such a chat would see it: Sam's chats have no folder, so they
        // are household chats — the coding gate hides the rule and the plain one comes whole.
        let row_in = |v: &Value, p: &str| -> Value {
            v["rules"]
                .as_array()
                .unwrap()
                .iter()
                .find(|r| r["path"] == p)
                .cloned()
                .unwrap_or(Value::Null)
        };
        let plain_preview = knowledge_rules_preview(&ctx, json!({"layer": sam})).unwrap();
        assert_eq!(row_in(&plain_preview, &tpath)["decision"], "hidden");
        assert_eq!(row_in(&plain_preview, &path)["decision"], "full");
        // An imagined coding chat running a push: the rule still waits at launch (its moment has
        // not fired there yet — no folder in it) but the call fires it.
        let fired = knowledge_rules_preview(
            &ctx,
            json!({"layer": sam, "kind": "coding", "tool": "Bash", "command": "git push origin"}),
        )
        .unwrap();
        assert_eq!(row_in(&fired, &tpath)["decision"], "hidden");
        assert_eq!(row_in(&fired, &tpath)["fires"], true);

        // Put Sam's list back to the one rule the copy/move section works with: drop the trigger
        // rule now that its previews have been read.
        knowledge_delete(&ctx, json!({"layer": sam, "kind": "rule", "name": tpath})).unwrap();

        // A copy keeps the original and lands in the other list; a taken name is refused; a move
        // removes the original.
        let copy = |from: &str, to: &str| {
            knowledge_copy(
                &ctx,
                json!({"kind": "rule", "name": path, "from": from, "to": to}),
            )
        };
        assert_eq!(copy(sam, alex).unwrap()["copied"], path);
        assert_eq!(titles(sam), [path.as_str()]);
        assert_eq!(titles(alex), [path.as_str()]);
        assert_eq!(
            knowledge_rule_read(&ctx, json!({"layer": alex, "path": path})).unwrap()["text"],
            "Nothing loud."
        );
        assert_eq!(copy(sam, alex).unwrap_err().code, "conflict");
        knowledge_move(
            &ctx,
            json!({"kind": "rule", "name": path, "from": sam, "to": "everyone"}),
        )
        .unwrap();
        assert!(titles(sam).is_empty());
        assert_eq!(titles("everyone"), [path.as_str()]);

        // Delete from the other person's list.
        let left =
            knowledge_delete(&ctx, json!({"layer": alex, "kind": "rule", "name": path})).unwrap();
        assert!(left["rules"].as_array().unwrap().is_empty());

        // A profile that is not on this PC is refused by every op, and no folder is made for it.
        let ghost = "profile:ghost";
        let refusals: Vec<(&str, ApiError)> = vec![
            (
                "get",
                knowledge_get(&ctx, json!({"layer": ghost})).unwrap_err(),
            ),
            (
                "read",
                knowledge_rule_read(&ctx, json!({"layer": ghost, "path": path})).unwrap_err(),
            ),
            (
                "write",
                knowledge_rule_write(&ctx, json!({"layer": ghost, "title": "T", "text": "x"}))
                    .unwrap_err(),
            ),
            (
                "delete",
                knowledge_delete(&ctx, json!({"layer": ghost, "kind": "rule", "name": path}))
                    .unwrap_err(),
            ),
            ("copy to", copy("everyone", ghost).unwrap_err()),
            ("copy from", copy(ghost, "everyone").unwrap_err()),
            ("same layer", copy(alex, alex).unwrap_err()),
        ];
        for (what, err) in refusals {
            assert_eq!(err.code, "bad_args", "{what}: {err}");
        }
        assert!(!data.join("knowledge/people/ghost").exists());
    }
}
