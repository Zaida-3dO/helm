//! Merged tools.
//!
//! A family of small tools (create, update, cancel and list a schedule) costs a chat the
//! description and schema of every member. A [`FacadeSpec`] puts the family behind one tool with
//! an `action` argument. The members stay registered under their own names: the facade does not
//! run anything itself. A call to the facade is rewritten to the member the action names, before
//! the gates run, so the restricted list, the parent-only rule, the size limit, the lock, the
//! timeout and every consent check see the member's own name and arguments, exactly as for a call
//! made by the old name. The old names keep working and are only left out of the catalogue.

use crate::registry::{RegisteredTool, ToolError};
use crate::spec::{AliasOf, CatalogueEntry, ToolSpec};
use serde::Serialize;
use serde_json::{json, Map, Value};

/// The longest description a facade, or any listed tool, carries. Longer usage notes go in
/// `guidelines`, which `app_list_tools` and `app_help` return on demand.
pub const MAX_LISTED_DESCRIPTION: usize = 480;

/// The first sentences of `text` that fit in `max` characters; the first sentence cut at a word
/// when it alone is longer.
pub fn lead(text: &str, max: usize) -> String {
    let flat = text.split_whitespace().collect::<Vec<_>>().join(" ");
    let mut end = 0;
    let mut from = 0;
    while let Some(i) = flat[from..].find(". ") {
        let at = from + i + 1;
        if flat[..at].chars().count() > max {
            break;
        }
        end = at;
        from = at + 1;
    }
    if end > 0 {
        return flat[..end].to_owned();
    }
    if flat.chars().count() <= max {
        return flat;
    }
    let cut: String = flat.chars().take(max - 1).collect();
    let cut = cut.rsplit_once(' ').map_or(cut.as_str(), |(h, _)| h);
    format!("{}…", cut.trim_end_matches([',', ';', ':', ' ']))
}

/// One merged tool.
#[derive(Clone, Copy, Debug)]
pub struct FacadeSpec {
    /// A reserved `app_` name that no tool registered under.
    pub name: &'static str,
    pub group: &'static str,
    /// What the family is for and what the actions are, in at most
    /// [`MAX_LISTED_DESCRIPTION`] characters.
    pub description: &'static str,
    /// Usage notes that do not fit the description (served by `app_help` and `app_list_tools`).
    pub guidelines: &'static str,
    /// Listed in every chat's own tool list.
    pub core: bool,
    /// `(action, member tool)`: the actions in the order they are listed.
    pub actions: &'static [(&'static str, &'static str)],
    /// `(action, argument, value)`: an action that is the member with one argument fixed, for a
    /// member that already takes its own `action` argument (so two actions may share a member).
    pub fixed: &'static [(&'static str, &'static str, &'static str)],
}

impl FacadeSpec {
    /// The argument an action fixes, if any.
    pub fn fixed_for(&self, action: &str) -> Option<(&'static str, &'static str)> {
        self.fixed
            .iter()
            .find(|(a, _, _)| *a == action)
            .map(|(_, k, v)| (*k, *v))
    }
}

/// One action of a merged tool, as `app_list_tools` shows it.
#[derive(Clone, Debug, PartialEq, Eq, Serialize)]
pub struct ActionInfo {
    pub action: String,
    /// The member tool's name: the old name, which still works.
    pub tool: String,
    pub description: String,
    /// The arguments the action cannot do without, besides `action`.
    #[serde(skip_serializing_if = "Vec::is_empty")]
    pub required: Vec<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub guidelines: Option<String>,
}

fn required_of(schema: &Value) -> Vec<String> {
    schema
        .get("required")
        .and_then(Value::as_array)
        .map(|r| {
            r.iter()
                .filter_map(Value::as_str)
                .map(str::to_owned)
                .collect()
        })
        .unwrap_or_default()
}

fn tagged(desc: Option<&str>, actions: &[&str], all: usize) -> Option<String> {
    if actions.len() == all {
        return desc.map(str::to_owned);
    }
    let tag = format!("[{}]", actions.join("|"));
    Some(match desc {
        Some(d) => format!("{tag} {d}"),
        None => tag,
    })
}

/// The one schema of a merged tool: `action` plus every argument of every member, each tagged
/// with the actions that take it. An argument two members define differently becomes an `anyOf`.
fn merged_schema(actions: &[(&str, &ToolSpec, Option<&str>)]) -> Value {
    // name -> [(definition without its description, description, actions)]
    #[allow(clippy::type_complexity)]
    let mut props: Vec<(String, Vec<(Value, Option<String>, Vec<&str>)>)> = Vec::new();
    for (action, spec, fixed) in actions {
        let Some(p) = spec
            .input_schema
            .get("properties")
            .and_then(Value::as_object)
        else {
            continue;
        };
        for (k, v) in p {
            if Some(k.as_str()) == *fixed {
                continue;
            }
            assert!(
                k != "action",
                "{}: a member takes an `action` argument",
                spec.name
            );
            let mut def = v.clone();
            let desc = def
                .as_object_mut()
                .and_then(|o| o.remove("description"))
                .and_then(|d| d.as_str().map(str::to_owned));
            let slot = match props.iter_mut().find(|(n, _)| n == k) {
                Some(s) => s,
                None => {
                    props.push((k.clone(), Vec::new()));
                    props.last_mut().unwrap()
                }
            };
            match slot
                .1
                .iter_mut()
                .find(|(d, ds, _)| *d == def && *ds == desc)
            {
                Some(variant) => variant.2.push(action),
                None => slot.1.push((def, desc, vec![action])),
            }
        }
    }
    let all = actions.len();
    let names: Vec<&str> = actions.iter().map(|(a, _, _)| *a).collect();
    let mut properties = Map::new();
    properties.insert(
        "action".into(),
        json!({"type": "string", "enum": names, "description": "What to do. Arguments are tagged with the actions that take them."}),
    );
    for (name, variants) in props {
        let mut rendered: Vec<Value> = variants
            .into_iter()
            .map(|(def, desc, acts)| {
                let mut o = def.as_object().cloned().unwrap_or_default();
                if let Some(d) = tagged(desc.as_deref(), &acts, all) {
                    o.insert("description".into(), Value::String(d));
                }
                Value::Object(o)
            })
            .collect();
        let value = if rendered.len() == 1 {
            rendered.remove(0)
        } else {
            json!({ "anyOf": rendered })
        };
        properties.insert(name, value);
    }
    json!({
        "type": "object",
        "properties": properties,
        "required": ["action"],
        "additionalProperties": false,
    })
}

/// The catalogue entry of a facade for the members a launch admits, or `None` when it admits none.
pub(crate) fn entry(
    f: &FacadeSpec,
    members: &[(&'static str, &RegisteredTool)],
) -> Option<CatalogueEntry> {
    if members.is_empty() {
        return None;
    }
    let specs: Vec<(&str, &ToolSpec, Option<&str>)> = members
        .iter()
        .map(|(a, t)| (*a, &t.spec, f.fixed_for(a).map(|(k, _)| k)))
        .collect();
    let infos: Vec<ActionInfo> = members
        .iter()
        .map(|(a, t)| {
            let fixed = f.fixed_for(a).map(|(k, _)| k);
            ActionInfo {
                action: (*a).to_owned(),
                tool: t.spec.name.to_owned(),
                description: t.spec.description.clone(),
                required: required_of(&t.spec.input_schema)
                    .into_iter()
                    .filter(|r| Some(r.as_str()) != fixed)
                    .collect(),
                guidelines: t.spec.guidelines.clone(),
            }
        })
        .collect();
    Some(CatalogueEntry {
        name: f.name.to_owned(),
        description: f.description.to_owned(),
        input_schema: merged_schema(&specs),
        guidelines: (!f.guidelines.is_empty()).then(|| f.guidelines.to_owned()),
        request_limit_bytes: members
            .iter()
            .map(|(_, t)| t.spec.request_limit.bytes())
            .max()
            .unwrap_or(0),
        timeout_ms: members
            .iter()
            .map(|(_, t)| t.spec.timeout_ms)
            .max()
            .unwrap_or(0),
        children: members.iter().all(|(_, t)| t.spec.availability.children),
        mutating: members.iter().any(|(_, t)| t.spec.mutating),
        group: f.group.to_owned(),
        core: f.core,
        core_with_orchestrator: false,
        alias_of: None,
        actions: infos,
    })
}

/// The alias marker for a member the facade took over.
pub(crate) fn alias(f: &FacadeSpec, action: &str) -> AliasOf {
    AliasOf {
        tool: f.name.to_owned(),
        action: action.to_owned(),
    }
}

/// Rewrite a call to a facade into the call to the member its `action` names. `Ok(None)` when
/// `name` is not a facade's.
pub(crate) fn rewrite(f: &FacadeSpec, args: &Value) -> Result<(&'static str, Value), ToolError> {
    let names = || {
        f.actions
            .iter()
            .map(|(a, _)| *a)
            .collect::<Vec<_>>()
            .join(", ")
    };
    let Some(action) = args.get("action").and_then(Value::as_str) else {
        return Err(ToolError::refused(format!(
            "{} needs an `action`: one of {}.",
            f.name,
            names()
        )));
    };
    let Some((_, member)) = f.actions.iter().find(|(a, _)| *a == action) else {
        return Err(ToolError::refused(format!(
            "{} has no action {:?}; the actions are {}.",
            f.name,
            action.chars().take(40).collect::<String>(),
            names()
        )));
    };
    let mut inner = args.as_object().cloned().unwrap_or_default();
    inner.remove("action");
    if let Some((key, value)) = f.fixed_for(action) {
        inner.insert(key.to_owned(), Value::String(value.to_owned()));
    }
    Ok((member, Value::Object(inner)))
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    fn spec(name: &'static str, props: Value, required: &[&str]) -> ToolSpec {
        ToolSpec::write(
            name,
            "d",
            json!({"type": "object", "properties": props, "required": required}),
        )
    }

    #[test]
    fn the_merged_schema_tags_arguments_and_keeps_every_variant() {
        let a = spec(
            "app_a",
            json!({"id": {"type": "string", "description": "Which."}, "title": {"type": "string", "description": "Short."}}),
            &["id"],
        );
        let b = spec(
            "app_b",
            json!({"id": {"type": "string", "description": "Which."}, "title": {"type": "integer"}, "only": {"type": "boolean"}}),
            &[],
        );
        let s = merged_schema(&[("one", &a, None), ("two", &b, None)]);
        let p = &s["properties"];
        assert_eq!(p["action"]["enum"], json!(["one", "two"]));
        assert_eq!(
            p["id"]["description"], "Which.",
            "an argument both take is not tagged"
        );
        assert_eq!(p["only"]["description"], "[two]");
        let any = p["title"]["anyOf"].as_array().unwrap();
        assert_eq!(any.len(), 2);
        assert_eq!(any[0]["description"], "[one] Short.");
        assert_eq!(any[1]["type"], "integer");
        assert_eq!(s["required"], json!(["action"]));
    }

    #[test]
    fn a_lead_keeps_whole_sentences_and_cuts_a_long_first_one_at_a_word() {
        let t = "First sentence here. Second one follows. Third is far too long to fit in the limit we set for it.";
        assert_eq!(lead(t, 45), "First sentence here. Second one follows.");
        assert_eq!(lead(t, 25), "First sentence here.");
        assert_eq!(
            lead(
                "A very long opening sentence without any full stop inside it at all",
                30
            ),
            "A very long opening sentence…"
        );
        assert_eq!(lead("Short.", 30), "Short.");
    }

    #[test]
    fn a_call_is_rewritten_to_the_member_and_a_bad_action_says_what_is_valid() {
        let f = FacadeSpec {
            name: "app_x",
            group: "x",
            description: "X.",
            guidelines: "",
            core: false,
            actions: &[
                ("make", "app_make_x"),
                ("drop", "app_drop_x"),
                ("park", "app_drop_x"),
            ],
            fixed: &[("park", "mode", "park")],
        };
        let (tool, args) = rewrite(&f, &json!({"action": "drop", "id": "7"})).unwrap();
        assert_eq!(tool, "app_drop_x");
        assert_eq!(args, json!({"id": "7"}));
        let (tool, args) = rewrite(&f, &json!({"action": "park", "mode": "x"})).unwrap();
        assert_eq!(
            (tool, args),
            ("app_drop_x", json!({"mode": "park"})),
            "a fixed argument wins"
        );
        for bad in [json!({}), json!({"action": 3}), json!({"action": "nope"})] {
            let e = rewrite(&f, &bad).unwrap_err();
            assert!(e.public.contains("make, drop, park"), "{}", e.public);
            assert_eq!(e.changed, crate::registry::Changed::No);
        }
    }
}
