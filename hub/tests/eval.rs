//! Search quality, measured: a fixed set of invented documents and queries goes through the
//! hub's own index and search, and each query class must keep its recall@5 and MRR.
//!
//! The documents and queries are `tests/eval/fixtures.json`. A document lands the way a real
//! one does (memories through the store, everything else through an `ix_put` frame), with
//! vectors from an embedder and with the search run as `memory_search` runs it (the date phrase
//! read out of the query first). Three runs:
//!
//! - **keyword**: no vectors at all (what the hub does with no embedder);
//! - **fake**: a deterministic word-bucket embedder, so the vector path, fusion, dates and
//!   privacy are checked without a model. It knows the fixture's synonym groups, so it proves the
//!   pipeline, not the meaning;
//! - **real**: only when `HELM_EVAL_EMBED_URL` points at an OpenAI-style embeddings server
//!   (for example the embedder image's `http://127.0.0.1:18080/v1`); skipped otherwise.
//!   `HELM_EVAL_MODEL` names the model (default `nomic-embed-text`).
//!
//! `HELM_EVAL_OUT=<file>` writes the report as markdown. Run it with `--nocapture` to read it.
//!
//! A class that falls under its threshold in `fixtures.json` fails the test; a row marked
//! `known_gap` is reported and never gated. The floors sit just under what the hub scores today;
//! the "plan bar" column says whether a class reaches the target recall (`bar`), which is a goal,
//! not a gate. A doc whose writer made it private never shows in another viewer's results, whatever
//! the scores. The household shares one memory otherwise: the `person` class checks that "my",
//! "Ben's" and the like pick the right person's record when two people each keep one.

use base64::Engine;
use helm_hub::index::{self, Ctx, Query};
use helm_hub::store::{NewMemory, Store, Visibility};
use serde_json::{json, Value};
use std::collections::{BTreeMap, HashMap};
use std::fmt::Write as _;
use std::time::Instant;

const DAY: i64 = 86_400_000;
const MODEL: &str = "nomic-embed-text";
const FAKE_DIM: usize = 256;
/// What the hub puts in front of a query before it is embedded; documents carry no prefix.
const QUERY_PREFIX: &str = "search_query: ";

type Embedder = Box<dyn Fn(&str, bool) -> Vec<f32>>;

fn fixtures() -> Value {
    serde_json::from_str(include_str!("eval/fixtures.json")).expect("fixtures.json parses")
}

// ---- the embedders ----

/// Words that carry no meaning of their own in the fake embedder.
const STOP: &[&str] = &[
    "the", "and", "for", "are", "was", "that", "this", "with", "from", "how", "our", "you", "its",
    "has", "not", "all", "any", "can", "each", "get", "does", "did", "when", "which", "what",
    "will", "out", "into", "than", "then", "too",
];

fn stem(w: &str) -> String {
    let w = w.trim_end_matches('s');
    w.to_owned()
}

/// A bag of word buckets: a word and its synonyms share a bucket, so related texts point the
/// same way. Deterministic, 256 dimensions.
fn fake_embedder(synonyms: &Value) -> Embedder {
    let mut canon: HashMap<String, String> = HashMap::new();
    for group in synonyms.as_array().unwrap() {
        let words: Vec<String> = group
            .as_array()
            .unwrap()
            .iter()
            .map(|w| stem(w.as_str().unwrap()))
            .collect();
        for w in &words {
            canon.insert(w.clone(), words[0].clone());
        }
    }
    Box::new(move |text, _query| {
        let mut v = vec![0f32; FAKE_DIM];
        let lower = text.to_lowercase();
        for raw in lower.split(|c: char| !c.is_alphanumeric()) {
            if raw.len() < 3 || STOP.contains(&raw) {
                continue;
            }
            let s = stem(raw);
            let word = canon.get(&s).cloned().unwrap_or(s);
            let h = word.bytes().fold(0xcbf2_9ce4_8422_2325u64, |h, b| {
                (h ^ u64::from(b)).wrapping_mul(0x100_0000_01b3)
            });
            v[(h % FAKE_DIM as u64) as usize] += 1.0;
        }
        v
    })
}

/// An OpenAI-style embeddings server, asked one text at a time.
fn real_embedder(url: &str, model: &str) -> Embedder {
    let url = {
        let u = url.trim().trim_end_matches('/');
        if u.ends_with("/embeddings") {
            u.to_owned()
        } else if u.ends_with("/v1") {
            format!("{u}/embeddings")
        } else {
            format!("{u}/v1/embeddings")
        }
    };
    let (model, agent) = (
        model.to_owned(),
        ureq::Agent::new_with_config(
            ureq::Agent::config_builder()
                .timeout_global(Some(std::time::Duration::from_secs(60)))
                .build(),
        ),
    );
    Box::new(move |text, query| {
        let input = if query {
            format!("{QUERY_PREFIX}{text}")
        } else {
            text.to_owned()
        };
        let mut resp = agent
            .post(&url)
            .send_json(json!({ "model": model, "input": input }))
            .expect("the embedder answers");
        let body: Value = resp
            .body_mut()
            .read_json()
            .expect("the embedder sends JSON");
        body.pointer("/data/0/embedding")
            .and_then(Value::as_array)
            .expect("the embedder sends a vector")
            .iter()
            .map(|x| x.as_f64().unwrap() as f32)
            .collect()
    })
}

// ---- loading the fixture into a hub index ----

struct Loaded {
    store: Store,
    /// Hit title -> fixture id.
    ids: HashMap<String, String>,
}

fn ctx(person: i64) -> Ctx {
    Ctx {
        person,
        device: format!("device-{person}"),
        acts_for: vec![person],
        people: vec![1, 2],
    }
}

fn load(fx: &Value, embed: Option<&Embedder>) -> Loaded {
    let now = fx["now_ms"].as_i64().unwrap();
    let mut store = Store::open_in_memory().unwrap();
    let mut ids = HashMap::new();
    for d in fx["docs"].as_array().unwrap() {
        let (id, title, text) = (
            d["id"].as_str().unwrap(),
            d["title"].as_str().unwrap(),
            d["text"].as_str().unwrap(),
        );
        let owner = d["owner"].as_i64().unwrap();
        let at = now - d["days_ago"].as_i64().unwrap() * DAY;
        let kind = d["kind"].as_str().unwrap();
        assert!(
            ids.insert(title.to_owned(), id.to_owned()).is_none(),
            "titles are unique: {title}"
        );
        if kind == "memory" {
            store
                .insert(NewMemory {
                    title: title.into(),
                    body: text.into(),
                    owner: Some(owner),
                    visibility: if d["scope"] == "everyone" {
                        Visibility::Shared
                    } else {
                        Visibility::Private
                    },
                    embedding: embed.map(|e| helm_hub::store::Embedding {
                        model: MODEL.into(),
                        values: e(&format!("{title}\n\n{text}"), false),
                    }),
                    ..Default::default()
                })
                .unwrap();
            continue;
        }
        let mut chunk = json!({ "ord": 0, "text": text, "at": at });
        if let Some(e) = embed {
            let (scale, q) = index::quantize(&e(text, false));
            chunk["vec"] = json!(base64::engine::general_purpose::STANDARD.encode(q));
            chunk["scale"] = json!(scale);
            chunk["model"] = json!(MODEL);
        }
        let (source_kind, tier) = match kind {
            "note" => ("folder", "note"),
            "helm_chat" => ("helm_chats", "conversation"),
            "claude_code" => ("claude_code", "conversation"),
            "telegram" => ("chat_logs", "conversation"),
            other => panic!("unknown kind {other}"),
        };
        let frame = json!({
            "type": "ix_put", "id": 1, "kind": source_kind,
            "source": format!("device-{owner}:{source_kind}"),
            "docs": [{
                "key": id, "hash": format!("h-{id}"), "title": title, "locator": format!("{id}.md"),
                "tier": tier, "owner": owner.to_string(), "scope": d["scope"],
                "chunks": [chunk],
            }],
        });
        let r = index::answer(&mut store, &ctx(owner), "ix_put", &json!(1), &frame, MODEL);
        assert_eq!(r["results"][0]["status"], "inserted", "{r}");
    }
    Loaded { store, ids }
}

// ---- running the queries ----

struct Row {
    class: String,
    text: String,
    known_gap: bool,
    recall: f64,
    rr: f64,
    leaked: Vec<String>,
    top: Vec<String>,
    ms: f64,
}

fn run_queries(fx: &Value, loaded: &Loaded, embed: Option<&Embedder>) -> Vec<Row> {
    let now = fx["now_ms"].as_i64().unwrap();
    let strings = |v: &Value| -> Vec<String> {
        v.as_array()
            .map(|a| a.iter().map(|s| s.as_str().unwrap().to_owned()).collect())
            .unwrap_or_default()
    };
    let people: Vec<(i64, String)> = fx["people"]
        .as_array()
        .map(|a| {
            a.iter()
                .map(|p| {
                    (
                        p["id"].as_i64().unwrap(),
                        p["name"].as_str().unwrap().to_owned(),
                    )
                })
                .collect()
        })
        .unwrap_or_default();
    let mut rows = Vec::new();
    for q in fx["queries"].as_array().unwrap() {
        let raw = q["text"].as_str().unwrap();
        let started = Instant::now();
        // The date phrase comes out of the words and becomes a window, as `memory_search` does.
        let (stripped, window) = helm_hub::when::parse(raw, now, 0);
        let text = if stripped.trim().is_empty() {
            raw.to_owned()
        } else {
            stripped
        };
        // Whose records the words ask for ("my flight", "Ben's flight"), as `memory_search` reads them.
        let viewer = q["viewer"].as_i64().unwrap();
        let focus = helm_hub::who::resolve(raw, viewer, &people);
        let found = index::search(
            &loaded.store,
            &Query {
                viewer,
                focus,
                text: text.clone(),
                k: 10,
                vector: embed.map(|e| e(&text, true)),
                model: embed.map(|_| MODEL.to_owned()),
                window: window.map(|w| (w.from_ms, w.to_ms - 1)),
                now_ms: now,
                ..Default::default()
            },
        )
        .unwrap();
        if std::env::var("HELM_EVAL_DEBUG").is_ok() {
            eprintln!(
                "{raw:?} text={text:?} window={window:?} widened={}",
                found.widened
            );
            for h in &found.hits {
                eprintln!(
                    "   {:.4} {} kw={} vec={} at={:?}",
                    h.score, h.title, h.keyword, h.vector, h.at
                );
            }
        }
        let ms = started.elapsed().as_secs_f64() * 1000.0;
        let mut top: Vec<String> = Vec::new();
        for h in &found.hits {
            let id = loaded.ids[&h.title].clone();
            if !top.contains(&id) {
                top.push(id);
            }
        }
        let (expect, forbid) = (strings(&q["expect"]), strings(&q["forbid"]));
        let top5 = &top[..top.len().min(5)];
        let leaked: Vec<String> = forbid.iter().filter(|f| top.contains(f)).cloned().collect();
        let (recall, rr) = if expect.is_empty() {
            let clean = if leaked.is_empty() { 1.0 } else { 0.0 };
            (clean, clean)
        } else {
            let hit = expect.iter().filter(|e| top5.contains(e)).count();
            let first = top.iter().position(|t| expect.contains(t));
            (
                hit as f64 / expect.len() as f64,
                first.map_or(0.0, |p| 1.0 / (p as f64 + 1.0)),
            )
        };
        if std::env::var("HELM_EVAL_DEBUG").is_ok() && rr < 1.0 {
            eprintln!("BELOW rr={rr:.2} {raw:?} viewer {viewer}: wanted {expect:?}, got {top:?}");
        }
        rows.push(Row {
            class: q["class"].as_str().unwrap().to_owned(),
            text: raw.to_owned(),
            known_gap: q["known_gap"].as_bool().unwrap_or(false),
            recall: if leaked.is_empty() { recall } else { 0.0 },
            rr: if leaked.is_empty() { rr } else { 0.0 },
            leaked,
            top: top5.to_vec(),
            ms,
        });
    }
    rows
}

fn percentile(v: &mut [f64], p: f64) -> f64 {
    v.sort_by(|a, b| a.partial_cmp(b).unwrap());
    v[((v.len() - 1) as f64 * p).round() as usize]
}

/// The report for one run, and the classes that fell under their threshold.
fn report(name: &str, rows: &[Row], limits: &Value, bar: f64, out: &mut String) -> Vec<String> {
    let mut by: BTreeMap<&str, Vec<&Row>> = BTreeMap::new();
    for r in rows {
        by.entry(&r.class).or_default().push(r);
    }
    let mut failures = Vec::new();
    let _ = writeln!(
        out,
        "\n### {name}\n\n| class | queries | recall@5 | MRR | limit |\n|---|---|---|---|---|"
    );
    for (class, rs) in &by {
        let n = rs.len() as f64;
        let (recall, mrr) = (
            rs.iter().map(|r| r.recall).sum::<f64>() / n,
            rs.iter().map(|r| r.rr).sum::<f64>() / n,
        );
        let limit = limits.get(*class);
        let gated = rs.iter().all(|r| !r.known_gap) && limit.is_some();
        let shown = match limit {
            Some(l) if gated => format!("{} / {}", l["recall5"], l["mrr"]),
            _ => "report only".to_owned(),
        };
        let bar_met = if recall + 1e-9 >= bar { "met" } else { "below" };
        let _ = writeln!(
            out,
            "| {class} | {} | {recall:.2} | {mrr:.2} | {shown} | {bar_met} |",
            rs.len()
        );
        if let (true, Some(l)) = (gated, limit) {
            if recall + 1e-9 < l["recall5"].as_f64().unwrap()
                || mrr + 1e-9 < l["mrr"].as_f64().unwrap()
            {
                failures.push(format!(
                    "{name}/{class}: recall@5 {recall:.2}, MRR {mrr:.2} under {l}"
                ));
            }
        }
    }
    // A private doc in someone else's results is never a ranking matter.
    for r in rows.iter().filter(|r| !r.leaked.is_empty()) {
        failures.push(format!(
            "{name}: \"{}\" leaked {}",
            r.text,
            r.leaked.join(", ")
        ));
    }
    let mut ms: Vec<f64> = rows.iter().map(|r| r.ms).collect();
    let _ = writeln!(
        out,
        "\nsearch time per query: p50 {:.1} ms, p95 {:.1} ms\n\nMisses (recall under 1 or a leak):\n",
        percentile(&mut ms.clone(), 0.5),
        percentile(&mut ms, 0.95)
    );
    for r in rows.iter().filter(|r| r.recall < 1.0) {
        let _ = writeln!(
            out,
            "- [{}]{} \"{}\" -> {}{}",
            r.class,
            if r.known_gap { " (known gap)" } else { "" },
            r.text,
            r.top.join(", "),
            if r.leaked.is_empty() {
                String::new()
            } else {
                format!(" LEAKED {}", r.leaked.join(", "))
            }
        );
    }
    failures
}

#[test]
fn search_quality_holds_per_query_class() {
    let fx = fixtures();
    let mut out = String::from("## Search quality\n");
    let mut failures = Vec::new();
    let bar = fx["bar"].as_f64().unwrap();

    let none = load(&fx, None);
    let rows = run_queries(&fx, &none, None);
    failures.extend(report(
        "keyword only",
        &rows,
        &fx["thresholds"]["keyword"],
        bar,
        &mut out,
    ));

    let fake = fake_embedder(&fx["synonyms"]);
    let loaded = load(&fx, Some(&fake));
    let rows = run_queries(&fx, &loaded, Some(&fake));
    failures.extend(report(
        "fake embedder",
        &rows,
        &fx["thresholds"]["fake"],
        bar,
        &mut out,
    ));

    match std::env::var("HELM_EVAL_EMBED_URL")
        .ok()
        .filter(|u| !u.trim().is_empty())
    {
        Some(url) => {
            let model = std::env::var("HELM_EVAL_MODEL").unwrap_or_else(|_| MODEL.into());
            let real = real_embedder(&url, &model);
            let loaded = load(&fx, Some(&real));
            let rows = run_queries(&fx, &loaded, Some(&real));
            failures.extend(report(
                "real embedder",
                &rows,
                &fx["thresholds"]["real"],
                bar,
                &mut out,
            ));
        }
        None => out.push_str("\n### real embedder\n\nskipped: HELM_EVAL_EMBED_URL is not set\n"),
    }

    println!("{out}");
    if let Ok(path) = std::env::var("HELM_EVAL_OUT") {
        std::fs::write(path, &out).unwrap();
    }
    assert!(
        failures.is_empty(),
        "search quality regressed:\n{}",
        failures.join("\n")
    );
}
