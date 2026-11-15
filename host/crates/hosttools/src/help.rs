//! `app_help`: how to use Helm, one small topic at a time.
//!
//! Instead of putting every tool's usage notes, the orchestrator's workflow steps and a long list
//! of rules into the prompt of every chat, the prompt carries one line and a chat asks for what it
//! needs:
//!
//! - no topic: the namespaces, one line each;
//! - a namespace (`tool`): its topics, one line each;
//! - a topic (`tool/app_schedule`): its text, at most [`MAX_TOPIC_CHARS`] characters. A longer text
//!   is served in parts (`tool/app_create_chat#2`), each part naming the next.
//!
//! Namespaces: `tool` (every tool, from its own description and guidelines, and for a merged tool
//! one topic per action), `helm` (files shipped in `help/`), and any namespace a crate adds through
//! [`HelpProviders`] (`rule`, `skill`, `project`, `workflow`: read live, never copied).

use crate::lines::LaunchRecords;
use crate::registry::{HostToolRegistry, ToolCtx, ToolError, ToolResult};
use crate::spec::{LaunchKeys, ToolSpec};
use serde_json::{json, Value};
use std::collections::BTreeMap;
use std::sync::{Arc, Mutex};

/// The longest topic text one answer carries.
pub const MAX_TOPIC_CHARS: usize = 1_500;
/// The longest index line (a topic's summary).
pub const MAX_SUMMARY_CHARS: usize = 100;
/// Topics one index answer lists.
const INDEX_PAGE: usize = 60;

/// A namespace some crate answers: its index and its topics, read live.
pub trait HelpProvider: Send + Sync {
    /// The namespace, `a-z` only: `rule`, `skill`, `project`.
    fn namespace(&self) -> &'static str;
    /// One line for the list of namespaces.
    fn about(&self) -> &'static str;
    /// `(name, summary)` for each topic; a summary is cut to [`MAX_SUMMARY_CHARS`].
    fn index(&self, chat_id: &str) -> Vec<(String, String)>;
    /// The text of one topic, whole (it is cut into parts here), or `None`.
    fn topic(&self, chat_id: &str, name: &str) -> Option<String>;
}

/// The providers crates add; a service read at each call.
#[derive(Default)]
pub struct HelpProviders {
    list: Mutex<Vec<Arc<dyn HelpProvider>>>,
}

impl HelpProviders {
    /// Add a provider. A second one for the same namespace, or for a namespace the shipped
    /// topics own, panics (a wiring bug).
    pub fn register(&self, p: Arc<dyn HelpProvider>) {
        let ns = p.namespace();
        assert!(
            !matches!(ns, "tool" | "helm"),
            "help namespace {ns} is built in"
        );
        let mut l = self.list.lock().unwrap_or_else(|e| e.into_inner());
        assert!(
            l.iter().all(|x| x.namespace() != ns),
            "help namespace registered twice: {ns}"
        );
        l.push(p);
    }

    fn all(&self) -> Vec<Arc<dyn HelpProvider>> {
        self.list.lock().unwrap_or_else(|e| e.into_inner()).clone()
    }
}

/// One shipped topic file.
pub struct Shipped {
    pub namespace: &'static str,
    pub name: &'static str,
    pub text: &'static str,
}

/// Every topic file in `help/`. A test fails on a file that is missing here or a listed one that
/// is missing on disk.
pub const SHIPPED: &[Shipped] = &[
    Shipped {
        namespace: "helm",
        name: "pages",
        text: include_str!("../help/helm/pages.md"),
    },
    Shipped {
        namespace: "helm",
        name: "cards",
        text: include_str!("../help/helm/cards.md"),
    },
    Shipped {
        namespace: "helm",
        name: "tools",
        text: include_str!("../help/helm/tools.md"),
    },
    Shipped {
        namespace: "helm",
        name: "worktrees",
        text: include_str!("../help/helm/worktrees.md"),
    },
    Shipped {
        namespace: "helm",
        name: "consent",
        text: include_str!("../help/helm/consent.md"),
    },
];

/// A shipped file split into its one-line summary and its body.
pub fn split_front(text: &str) -> (String, String) {
    let t = text.trim_start_matches('\u{feff}');
    let Some(rest) = t
        .strip_prefix("---\n")
        .or_else(|| t.strip_prefix("---\r\n"))
    else {
        return (String::new(), t.trim().to_owned());
    };
    let Some((front, body)) = rest.split_once("\n---") else {
        return (String::new(), t.trim().to_owned());
    };
    let summary = front
        .lines()
        .find_map(|l| l.strip_prefix("summary:"))
        .map(|s| s.trim().to_owned())
        .unwrap_or_default();
    let body = body.trim_start_matches(['\r', '\n']).trim().to_owned();
    (summary, body)
}

/// Cut a text into parts of at most [`MAX_TOPIC_CHARS`] characters, at a blank line, a line end or
/// a space, whichever is nearest the limit.
pub fn parts(text: &str) -> Vec<String> {
    let mut out = Vec::new();
    let mut rest = text.trim();
    while rest.chars().count() > MAX_TOPIC_CHARS {
        let cut = rest
            .char_indices()
            .nth(MAX_TOPIC_CHARS)
            .map(|(i, _)| i)
            .unwrap_or(rest.len());
        let head = &rest[..cut];
        let at = ["\n\n", "\n", ". ", " "]
            .iter()
            .find_map(|sep| {
                head.rfind(sep)
                    .filter(|i| *i > cut / 2)
                    .map(|i| i + sep.len())
            })
            .unwrap_or(cut);
        out.push(rest[..at].trim().to_owned());
        rest = rest[at..].trim_start();
    }
    if !rest.is_empty() || out.is_empty() {
        out.push(rest.to_owned());
    }
    out
}

fn first_sentence(s: &str) -> String {
    let one_line = s.split_whitespace().collect::<Vec<_>>().join(" ");
    let end = one_line
        .char_indices()
        .find(|(i, c)| matches!(c, '.' | '!' | '?') && one_line[i + 1..].starts_with(' '))
        .map(|(i, _)| i + 1)
        .unwrap_or(one_line.len());
    cut_summary(&one_line[..end])
}

fn cut_summary(s: &str) -> String {
    if s.chars().count() <= MAX_SUMMARY_CHARS {
        return s.to_owned();
    }
    let head: String = s.chars().take(MAX_SUMMARY_CHARS - 1).collect();
    format!("{}…", head.trim_end())
}

/// What the `tool` namespace holds, from the registry: a topic per tool, per merged tool, and per
/// action of a merged tool, for the tools the chat's launch lists. A tool this launch does not
/// admit (one the worker itself calls, one of a switched-off integration, one a mode adds) has no
/// topic. The stubs for a merged tool's old names answer by their exact topic when `aliases` is
/// set and are left out of the index otherwise.
fn tool_topics(
    reg: &HostToolRegistry,
    keys: &LaunchKeys,
    aliases: bool,
) -> BTreeMap<String, (String, String)> {
    let mut out = BTreeMap::new();
    let admitted = |name: &str| {
        reg.get(name)
            .is_some_and(|t| keys.admits(t.spec.availability.when))
    };
    let text_of = |s: &ToolSpec| {
        let mut t = format!("{}: {}", s.name, s.description);
        if let Some(g) = &s.guidelines {
            t.push_str("\n\n");
            t.push_str(g);
        }
        t
    };
    for f in reg.facades() {
        if !f.actions.iter().any(|(_, m)| admitted(m)) {
            continue;
        }
        let mut text = format!("{}: {}", f.name, f.description);
        if !f.guidelines.is_empty() {
            text.push_str("\n\n");
            text.push_str(f.guidelines);
        }
        text.push_str("\n\nActions, in full: ");
        let acts: Vec<String> = f
            .actions
            .iter()
            .filter(|(_, m)| admitted(m))
            .map(|(a, _)| format!("tool/{}/{a}", f.name))
            .collect();
        text.push_str(&acts.join(", "));
        out.insert(
            format!("tool/{}", f.name),
            (first_sentence(f.description), text),
        );
        for (action, member) in f.actions {
            if let Some(t) = reg.get(member).filter(|_| admitted(member)) {
                let mut text = text_of(&t.spec);
                text.push_str(&format!(
                    "\n\n(Call it as {} {{\"action\": \"{action}\", ...}}; the old name {member} still works.)",
                    f.name
                ));
                out.insert(
                    format!("tool/{}/{action}", f.name),
                    (first_sentence(&t.spec.description), text),
                );
            }
        }
    }
    for name in reg.names() {
        let Some(t) = reg.get(name) else { continue };
        if !keys.admits(t.spec.availability.when) {
            continue;
        }
        if let Some((f, action)) = reg.alias_of(name) {
            if !aliases {
                continue;
            }
            out.insert(
                format!("tool/{name}"),
                (
                    format!("Now {} {{action: {action}}}", f.name),
                    format!(
                        "{name} is now {} with action \"{action}\". It still works by its old name.\n\nSee tool/{}/{action}.",
                        f.name, f.name
                    ),
                ),
            );
        } else {
            out.insert(
                format!("tool/{name}"),
                (first_sentence(&t.spec.description), text_of(&t.spec)),
            );
        }
    }
    out
}

fn shipped(ns: &str) -> BTreeMap<String, (String, String)> {
    SHIPPED
        .iter()
        .filter(|s| s.namespace == ns)
        .map(|s| {
            let (summary, body) = split_front(s.text);
            (format!("{ns}/{}", s.name), (summary, body))
        })
        .collect()
}

/// The answer to one `app_help` call.
pub fn answer(
    reg: &HostToolRegistry,
    providers: Option<&HelpProviders>,
    chat_id: &str,
    keys: &LaunchKeys,
    topic: Option<&str>,
    offset: usize,
) -> ToolResult {
    let providers = providers.map(HelpProviders::all).unwrap_or_default();
    let topic = topic.map(str::trim).filter(|t| !t.is_empty());
    let Some(topic) = topic else {
        let mut namespaces = vec![
            json!({"namespace": "tool", "topics": tool_topics(reg, keys, false).len(), "about": "How to call a Helm tool: when to use it, its arguments, its actions"}),
            json!({"namespace": "helm", "topics": shipped("helm").len(), "about": "Pages, cards, worktrees, consent: how Helm's own features work"}),
        ];
        for p in &providers {
            namespaces.push(json!({"namespace": p.namespace(), "topics": p.index(chat_id).len(), "about": p.about()}));
        }
        return Ok(json!({
            "namespaces": namespaces,
            "next": "app_help {topic: \"<namespace>\"} lists its topics; {topic: \"<namespace>/<name>\"} reads one."
        }));
    };
    // `topic#2` is the second part of a long topic.
    let (name, part) = match topic.split_once('#') {
        Some((n, p)) => (n, p.parse::<usize>().ok().filter(|p| *p >= 1)),
        None => (topic, Some(1)),
    };
    let Some(part) = part else {
        return Err(ToolError::refused(
            "A part number follows # and starts at 1, as in tool/app_create_chat#2.",
        ));
    };
    let (ns, rest) = name.split_once('/').unwrap_or((name, ""));
    let found: Option<(String, String)> = match ns {
        "tool" | "helm" => {
            let all = if ns == "tool" {
                tool_topics(reg, keys, !rest.is_empty())
            } else {
                shipped("helm")
            };
            if rest.is_empty() {
                return Ok(index(
                    ns,
                    all.into_iter().map(|(k, (sum, _))| (k, sum)).collect(),
                    offset,
                ));
            }
            all.get(name).cloned()
        }
        _ => match providers.iter().find(|p| p.namespace() == ns) {
            None => None,
            Some(p) if rest.is_empty() => return Ok(index(ns, p.index(chat_id), offset)),
            Some(p) => p.topic(chat_id, rest).map(|t| (String::new(), t)),
        },
    };
    let Some((_, text)) = found else {
        return Err(ToolError::refused(format!(
            "There is no help topic {}. Call app_help with no topic to see the namespaces.",
            name.chars().take(80).collect::<String>()
        )));
    };
    let all = parts(&text);
    let Some(body) = all.get(part - 1) else {
        return Err(ToolError::refused(format!(
            "{name} has {} part(s).",
            all.len()
        )));
    };
    let mut out = json!({"topic": name, "text": body});
    if part < all.len() {
        out["continues"] = json!(format!("{name}#{}", part + 1));
    }
    Ok(out)
}

fn index(ns: &str, mut topics: Vec<(String, String)>, offset: usize) -> Value {
    topics.sort();
    let total = topics.len();
    let page: Vec<Value> = topics
        .into_iter()
        .skip(offset)
        .take(INDEX_PAGE)
        .map(|(n, s)| {
            let full = if n.starts_with(&format!("{ns}/")) {
                n
            } else {
                format!("{ns}/{n}")
            };
            json!({"topic": full, "summary": cut_summary(&s)})
        })
        .collect();
    let mut out = json!({"namespace": ns, "total": total, "topics": page});
    if offset + INDEX_PAGE < total {
        out["nextOffset"] = json!(offset + INDEX_PAGE);
    }
    out
}

/// Register `app_help`. It answers from a snapshot of the registry as it stands when this is
/// called, so call it after every other tool and merged tool is registered.
pub fn register(reg: &mut HostToolRegistry) {
    let snapshot = Arc::new(reg.clone());
    reg.blocking(
        ToolSpec::read(
            "app_help",
            "How to use Helm's tools and features. No topic: the namespaces. A namespace (tool): its topics, one line each. A topic (tool/app_schedule): its text. Use it before calling a tool you are unsure of; a long topic continues at the part it names.",
            json!({
                "type": "object",
                "properties": {
                    "topic": {"type": "string", "maxLength": 120, "description": "A namespace, or namespace/name, optionally #part."},
                    "offset": {"type": "integer", "minimum": 0, "description": "From a previous index's nextOffset."}
                },
                "additionalProperties": false
            }),
        )
        .children(),
        move |ctx: &ToolCtx, args: Value| {
            let providers = ctx.services.get::<HelpProviders>();
            // What this chat's launch lists; a chat with no record gets the always-on tools only.
            let keys = ctx
                .services
                .get::<LaunchRecords>()
                .and_then(|r| r.0.keys_of(&ctx.caller_chat_id))
                .unwrap_or_default();
            let offset = args
                .get("offset")
                .and_then(Value::as_u64)
                .unwrap_or(0)
                .min(10_000) as usize;
            answer(
                &snapshot,
                providers.as_deref(),
                &ctx.caller_chat_id,
                &keys,
                args.get("topic").and_then(Value::as_str),
                offset,
            )
        },
    );
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::facade::FacadeSpec;
    use crate::registry::ToolResult;
    use crate::spec::Condition;

    fn ok(_: &ToolCtx, _: Value) -> ToolResult {
        Ok(Value::Null)
    }

    fn reg() -> HostToolRegistry {
        let obj = json!({"type": "object", "properties": {}});
        let mut r = HostToolRegistry::new();
        r.blocking(
            ToolSpec::read(
                "app_list_schedules",
                "List the schedules. Each has an id.",
                obj.clone(),
            ),
            ok,
        )
        .blocking(
            ToolSpec::write("app_cancel_schedule", "Cancel one.", obj.clone())
                .guidelines("x".repeat(3_200)),
            ok,
        )
        .blocking(
            ToolSpec::read("app_this_chat", "This chat.", obj.clone()),
            ok,
        )
        .blocking(
            ToolSpec::read("app_workflow_gate", "Agents never see it.", obj.clone())
                .when(Condition::Worker),
            ok,
        )
        .blocking(
            ToolSpec::read("app_gmail_search", "Mail.", obj).when(Condition::GmailAccounts),
            ok,
        )
        .facade(FacadeSpec {
            name: "app_schedule",
            group: "schedules",
            description: "Schedules. Make, list or cancel.",
            guidelines: "Only when asked.",
            core: false,
            actions: &[
                ("list", "app_list_schedules"),
                ("cancel", "app_cancel_schedule"),
            ],
            fixed: &[],
        });
        r
    }

    struct Rules;
    impl HelpProvider for Rules {
        fn namespace(&self) -> &'static str {
            "rule"
        }
        fn about(&self) -> &'static str {
            "Your rules"
        }
        fn index(&self, _: &str) -> Vec<(String, String)> {
            vec![("search".into(), "Search safely.".into())]
        }
        fn topic(&self, _: &str, name: &str) -> Option<String> {
            (name == "search").then(|| "Keep every search small.".to_owned())
        }
    }

    #[test]
    fn the_namespaces_indexes_topics_and_parts_answer_and_say_what_is_missing() {
        let r = reg();
        let p = HelpProviders::default();
        p.register(Arc::new(Rules));
        let ask = |t: Option<&str>| answer(&r, Some(&p), "c", &LaunchKeys::default(), t, 0);
        let v = ask(None).unwrap();
        let names: Vec<_> = v["namespaces"]
            .as_array()
            .unwrap()
            .iter()
            .map(|n| n["namespace"].as_str().unwrap())
            .collect();
        assert_eq!(names, ["tool", "helm", "rule"]);
        // A namespace lists the topics this launch admits; an old name of a merged tool is not
        // indexed but answers by its exact topic, pointing at the merged tool.
        let v = ask(Some("tool")).unwrap();
        let topics: BTreeMap<_, _> = v["topics"]
            .as_array()
            .unwrap()
            .iter()
            .map(|t| (t["topic"].as_str().unwrap(), t["summary"].as_str().unwrap()))
            .collect();
        assert_eq!(topics["tool/app_schedule"], "Schedules.");
        assert!(!topics.contains_key("tool/app_cancel_schedule"));
        assert!(topics.contains_key("tool/app_schedule/list"));
        assert!(ask(Some("tool/app_cancel_schedule")).unwrap()["text"]
            .as_str()
            .unwrap()
            .contains("is now app_schedule"));
        // A worker-only tool and a switched-off integration's tool are neither indexed nor read;
        // the integration's tool appears once the launch lists it.
        for hidden in ["tool/app_workflow_gate", "tool/app_gmail_search"] {
            assert!(!topics.contains_key(hidden), "{hidden}");
            assert!(ask(Some(hidden)).is_err(), "{hidden}");
        }
        let gmail = LaunchKeys {
            gmail_accounts: true,
            ..Default::default()
        };
        assert!(answer(&r, Some(&p), "c", &gmail, Some("tool/app_gmail_search"), 0).is_ok());
        assert!(answer(&r, Some(&p), "c", &gmail, Some("tool/app_workflow_gate"), 0).is_err());
        assert_eq!(
            ask(Some("rule")).unwrap()["topics"][0]["topic"],
            "rule/search"
        );
        assert_eq!(
            ask(Some("rule/search")).unwrap()["text"],
            "Keep every search small."
        );
        // A merged tool's topic names its actions; an action's topic is the member's own text.
        let t = ask(Some("tool/app_schedule")).unwrap();
        assert!(t["text"]
            .as_str()
            .unwrap()
            .contains("tool/app_schedule/cancel"));
        // Long text is served in parts of at most the cap, each naming the next.
        let first = ask(Some("tool/app_schedule/cancel")).unwrap();
        assert!(first["text"].as_str().unwrap().chars().count() <= MAX_TOPIC_CHARS);
        assert_eq!(first["continues"], "tool/app_schedule/cancel#2");
        let last = ask(Some("tool/app_schedule/cancel#3")).unwrap();
        assert!(last.get("continues").is_none());
        for bad in [
            "tool/nope",
            "rule/none",
            "nowhere/x",
            "tool/app_schedule/cancel#9",
            "tool/app_schedule#0",
        ] {
            let e = ask(Some(bad)).unwrap_err();
            assert_eq!(e.changed, crate::registry::Changed::No, "{bad}");
        }
    }

    #[test]
    fn parts_cut_at_a_line_end_and_lose_nothing() {
        let text = (0..60)
            .map(|i| format!("Line {i} says something plain."))
            .collect::<Vec<_>>()
            .join("\n");
        let got = parts(&text);
        assert!(got.len() > 1);
        assert!(got.iter().all(|p| p.chars().count() <= MAX_TOPIC_CHARS));
        assert_eq!(got.join("\n"), text);
        assert_eq!(parts("short"), ["short"]);
        let (s, b) = split_front("---\nsummary: One line.\n---\nBody here.\n");
        assert_eq!((s.as_str(), b.as_str()), ("One line.", "Body here."));
    }

    #[test]
    fn every_shipped_topic_fits_and_is_listed() {
        let dir = std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("help");
        let mut on_disk = Vec::new();
        for ns in std::fs::read_dir(&dir).unwrap().flatten() {
            for f in std::fs::read_dir(ns.path()).unwrap().flatten() {
                on_disk.push(format!(
                    "{}/{}",
                    ns.file_name().to_string_lossy(),
                    f.path().file_stem().unwrap().to_string_lossy()
                ));
            }
        }
        on_disk.sort();
        let listed: Vec<String> = SHIPPED
            .iter()
            .map(|s| format!("{}/{}", s.namespace, s.name))
            .collect();
        let mut sorted = listed.clone();
        sorted.sort();
        assert_eq!(on_disk, sorted, "help/ and SHIPPED list the same files");
        let known: Vec<&str> = listed.iter().map(String::as_str).collect();
        for s in SHIPPED {
            let (summary, body) = split_front(s.text);
            let id = format!("{}/{}", s.namespace, s.name);
            assert!(
                !summary.is_empty() && summary.chars().count() <= MAX_SUMMARY_CHARS,
                "{id} summary"
            );
            assert!(
                !body.is_empty() && body.chars().count() <= MAX_TOPIC_CHARS,
                "{id} is {} characters",
                body.chars().count()
            );
            // A topic named in a body is one that exists (a tool topic is checked by name only).
            for w in body
                .split(|c: char| !(c.is_ascii_alphanumeric() || c == '/' || c == '_' || c == '-'))
            {
                if w.starts_with("helm/") {
                    assert!(
                        known.contains(&w),
                        "{id} points at {w}, which does not exist"
                    );
                }
            }
        }
    }
}
