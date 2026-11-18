//! The broker's doctor, behind `mcp_broker_doctor`.
//!
//! Settings → MCP shows one status row for the built-in browser server: is the broker installed,
//! is the browser extension reachable, is its token set. The broker itself answers that, and
//! answers it completely: this module only starts `broker.js doctor` beside the tool entry the
//! server names, with that server's own settings in its environment, and reports what comes back.
//! The doctor changes nothing on its own; it never launches or repairs anything, so asking costs
//! a process and a store read.
//!
//! The rules:
//! - Only the browser preset's stdio server is asked. Any other server is refused with a plain
//!   line, and nothing spawns.
//! - The environment of the saved server is handed to the child exactly as for a connection test;
//!   the same credential-stripping rules apply. No env value, and no resolved secret, may appear
//!   in the answer: every returned string is scrubbed of the server's resolved secrets first.
//! - The broker reports each precondition on its own line and deliberately offers no single
//!   verdict; `ok` here only means "nothing failed and nothing was unknown", and the checks list
//!   stays whole either way.

use crate::config::McpStore;
use crate::presets::BROKER_PRESET_ID;
use crate::probe::{plan_spawn, STRIPPED_ENV};
use crate::types::TransportKind;
use serde::{Deserialize, Serialize};
use std::io::Read;
use std::path::PathBuf;
use std::process::{Command, Stdio};
use std::sync::mpsc;
use std::thread;
use std::time::{Duration, Instant};
use ts_rs::TS;

/// How long to wait for the doctor to answer.
const DOCTOR_TIMEOUT: Duration = Duration::from_secs(20);
/// The whole doctor report is small; a longer stdout means something else was printed.
const MAX_REPORT_BYTES: usize = 64 * 1024;

/// One precondition, as the broker's doctor reported it.
#[derive(Clone, Debug, PartialEq, Serialize, TS)]
#[serde(rename_all = "camelCase")]
pub struct DoctorCheck {
    pub id: String,
    pub title: String,
    /// `ok`, `failed` or `unknown` — the broker's own words, kept as they came.
    pub status: String,
    pub detail: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    #[ts(optional)]
    pub remedy: Option<String>,
}

/// What `mcp_broker_doctor` answers: either the checks, or one line saying why there are none.
#[derive(Clone, Debug, PartialEq, Serialize, TS)]
#[serde(rename_all = "camelCase")]
pub struct DoctorOutcome {
    /// Nothing failed and nothing was unknown. False also when the run did not answer.
    pub ok: bool,
    /// One line for the row: the count, or why nothing came back.
    pub message: String,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    #[ts(as = "Option<Vec<DoctorCheck>>", optional)]
    pub checks: Vec<DoctorCheck>,
}

/// What `mcp_broker_doctor` takes.
#[derive(Deserialize, Serialize, TS)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct DoctorArgs {
    pub server_id: String,
}

fn outcome(ok: bool, message: impl Into<String>, checks: Vec<DoctorCheck>) -> DoctorOutcome {
    DoctorOutcome {
        ok,
        message: message.into(),
        checks,
    }
}

/// The doctor program that sits beside a broker tool entry: `broker-tool.js` becomes
/// `broker.js` in the same folder, same extension. `None` when the arguments do not name a
/// broker tool entry at all.
pub fn doctor_entry(args: &[String]) -> Option<PathBuf> {
    for a in args {
        let p = std::path::Path::new(a);
        let stem = p.file_stem()?.to_str()?.to_ascii_lowercase();
        let ext = p.extension()?.to_str()?.to_ascii_lowercase();
        if stem == "broker-tool" && matches!(ext.as_str(), "js" | "mjs" | "cjs") {
            let dir = p.parent().unwrap_or_else(|| std::path::Path::new(""));
            return Some(dir.join(format!("broker.{ext}")));
        }
    }
    None
}

/// The broker's own JSON report, of which only the checks are shown.
#[derive(Deserialize)]
struct BrokerReport {
    #[serde(default)]
    checks: Vec<BrokerCheck>,
}

#[derive(Deserialize)]
struct BrokerCheck {
    #[serde(default)]
    id: String,
    #[serde(default)]
    title: String,
    #[serde(default)]
    status: String,
    #[serde(default)]
    detail: String,
    #[serde(default)]
    remedy: Option<String>,
}

/// Parse the report the broker prints. `None` when the stdout is not its JSON report.
pub fn parse_report(stdout: &str) -> Option<Vec<DoctorCheck>> {
    let r: BrokerReport = serde_json::from_str(stdout.trim()).ok()?;
    if r.checks.is_empty() {
        return None;
    }
    Some(
        r.checks
            .into_iter()
            .map(|c| DoctorCheck {
                id: c.id,
                title: c.title,
                status: c.status,
                detail: c.detail,
                remedy: c.remedy,
            })
            .collect(),
    )
}

/// The counts the row shows. Unknown counts as not-passed: "could not tell" is not "fine".
fn summarise(checks: &[DoctorCheck]) -> (bool, String) {
    let bad = checks.iter().filter(|c| c.status != "ok").count();
    if bad == 0 {
        (true, format!("All {} checks passed", checks.len()))
    } else {
        (
            false,
            format!("{} of {} checks did not pass", bad, checks.len()),
        )
    }
}

/// Remove every resolved secret from a string the user will read. The broker is not expected to
/// echo one; the scrub is here so a slip cannot reach the screen anyway.
fn scrub(text: &str, secrets: &[String]) -> String {
    // The worker's set: a secret may also come back JSON-escaped or percent-encoded.
    let mut all: Vec<String> = Vec::new();
    for s in secrets.iter().filter(|s| s.len() >= 4) {
        all.push(s.clone());
        if let Ok(j) = serde_json::to_string(s) {
            all.push(j[1..j.len() - 1].to_owned());
        }
        all.push(url_encoded(s));
    }
    // Longest first, so a secret that contains another is replaced whole.
    all.sort_by_key(|s| std::cmp::Reverse(s.len()));
    all.dedup();
    let mut out = text.to_owned();
    for s in all {
        out = out.replace(s.as_str(), "•••");
    }
    out
}

/// `encodeURIComponent`: everything but letters, digits and `-_.!~*'()` as percent escapes.
fn url_encoded(s: &str) -> String {
    let mut out = String::new();
    for b in s.bytes() {
        if b.is_ascii_alphanumeric() || b"-_.!~*'()".contains(&b) {
            out.push(b as char);
        } else {
            out.push_str(&format!("%{b:02X}"));
        }
    }
    out
}

/// Ask the broker about the saved browser server. Never fails the op: a refusal to ask, a dead
/// program and a broken answer all come back as `ok: false` with the reason.
pub fn run(store: &McpStore, server_id: &str) -> DoctorOutcome {
    let servers = match store.load() {
        Ok(s) => s,
        Err(e) => {
            return outcome(
                false,
                format!("The saved servers could not be read: {e}"),
                vec![],
            )
        }
    };
    let Some(s) = servers.into_iter().find(|s| s.id == server_id) else {
        return outcome(false, "That server no longer exists.", vec![]);
    };
    if s.preset.as_deref() != Some(BROKER_PRESET_ID) || s.transport.kind != TransportKind::Stdio {
        return outcome(
            false,
            "Only the browser server can be checked this way.",
            vec![],
        );
    }
    if !s.enabled {
        return outcome(false, "Turn the browser server on first.", vec![]);
    }
    let Some(program) = s.transport.command.as_deref() else {
        return outcome(false, "The server has no program.", vec![]);
    };
    let Some(entry) = doctor_entry(&s.transport.args) else {
        return outcome(
            false,
            "The arguments must name the broker tool entry (broker-tool.js).",
            vec![],
        );
    };
    let resolved = store.resolve_draft(&s);
    if let Some(e) = &resolved.error {
        return outcome(false, e.clone(), vec![]);
    }
    let node = helm_workers::node::resolve_node(&helm_workers::node::NodeLookup::from_env())
        .ok()
        .map(|n| n.path);
    let args = vec![entry.to_string_lossy().into_owned(), "doctor".into()];
    let (exe, args) = match plan_spawn(
        program,
        &args,
        &std::env::var_os("PATH").unwrap_or_default(),
        node.as_deref(),
        cfg!(windows),
    ) {
        Ok(p) => p,
        Err(why) => return outcome(false, why, vec![]),
    };
    let mut cmd = Command::new(&exe);
    cmd.args(args)
        .envs(&resolved.env)
        .stdin(Stdio::null())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped());
    for k in STRIPPED_ENV {
        cmd.env_remove(k);
    }
    #[cfg(windows)]
    {
        use std::os::windows::process::CommandExt;
        const CREATE_NO_WINDOW: u32 = 0x0800_0000;
        cmd.creation_flags(CREATE_NO_WINDOW);
    }
    let mut child = match cmd.spawn() {
        Ok(c) => c,
        Err(e) => {
            return outcome(
                false,
                format!("Could not start the broker's doctor: {e}"),
                vec![],
            )
        }
    };
    let (tx, rx) = mpsc::channel::<Vec<u8>>();
    if let Some(mut out) = child.stdout.take() {
        thread::spawn(move || {
            let mut buf = Vec::new();
            let _ = out
                .by_ref()
                .take(MAX_REPORT_BYTES as u64)
                .read_to_end(&mut buf);
            let _ = tx.send(buf);
        });
    }
    let (etx, erx) = mpsc::channel::<String>();
    if let Some(mut err) = child.stderr.take() {
        thread::spawn(move || {
            // Read a fair amount: the cut comes after the scrub, so a secret is never split by it.
            let mut buf = Vec::new();
            let _ = err.by_ref().take(8 * 1024).read_to_end(&mut buf);
            let _ = etx.send(String::from_utf8_lossy(&buf).into_owned());
        });
    }
    let deadline = Instant::now() + DOCTOR_TIMEOUT;
    let stdout = match rx.recv_timeout(deadline.saturating_duration_since(Instant::now())) {
        Ok(b) => b,
        Err(_) => {
            let _ = child.kill();
            let _ = child.wait();
            return outcome(false, "The broker's doctor did not answer in time.", vec![]);
        }
    };
    let _ = child.wait();
    let said = erx
        .recv_timeout(Duration::from_millis(200))
        .unwrap_or_default();
    let text = String::from_utf8_lossy(&stdout);
    let Some(checks) = parse_report(&text) else {
        let said = scrub(said.trim(), &resolved.secrets);
        let said: String = said.chars().take(300).collect();
        return outcome(
            false,
            if said.is_empty() {
                "The broker's doctor did not answer properly.".into()
            } else {
                format!("The broker's doctor did not answer properly. It said: {said}")
            },
            vec![],
        );
    };
    let checks: Vec<DoctorCheck> = checks
        .into_iter()
        .map(|mut c| {
            c.detail = scrub(&c.detail, &resolved.secrets);
            c.remedy = c.remedy.map(|r| scrub(&r, &resolved.secrets));
            c
        })
        .collect();
    let (ok, message) = summarise(&checks);
    outcome(ok, message, checks)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_doctor_entry_sits_beside_the_tool_entry() {
        let mk = |a: &str| doctor_entry(&[a.into()]);
        assert_eq!(
            mk("C:\\bb\\dist\\src\\bin\\broker-tool.js"),
            Some(PathBuf::from("C:\\bb\\dist\\src\\bin\\broker.js"))
        );
        assert_eq!(
            mk("/bb/dist/src/bin/broker-tool.mjs"),
            Some(PathBuf::from("/bb/dist/src/bin/broker.mjs"))
        );
        assert_eq!(mk("bb/broker-tool"), None, "no extension is not an entry");
        assert_eq!(mk("bb/other.js"), None);
        assert_eq!(mk("bb/echo.js"), None);
        assert_eq!(doctor_entry(&[]), None);
    }

    #[test]
    fn a_report_parses_and_a_non_report_does_not() {
        let good = r#"{"checks":[{"id":"attached.token","title":"The token is set","status":"ok","detail":"set"},{"id":"attached.binary","title":"The browser binary is where it should be","status":"failed","detail":"not found","remedy":"install it"}],"exitCode":1}"#;
        let checks = parse_report(good).unwrap();
        assert_eq!(checks.len(), 2);
        assert_eq!(checks[1].status, "failed");
        assert_eq!(checks[1].remedy.as_deref(), Some("install it"));
        let (ok, msg) = summarise(&checks);
        assert!(!ok);
        assert_eq!(msg, "1 of 2 checks did not pass");
        let all_ok =
            parse_report(r#"{"checks":[{"id":"a","title":"A","status":"ok","detail":"fine"}]}"#)
                .unwrap();
        assert!(summarise(&all_ok).0);
        assert!(parse_report("not json at all").is_none());
        assert!(parse_report(r#"{"checks":[]}"#).is_none());
    }

    #[test]
    fn the_scrub_catches_a_secret_before_the_screen() {
        assert_eq!(
            scrub("token tok_secret here", &["tok_secret".into()]),
            "token ••• here"
        );
        // Its JSON-escaped and percent-encoded forms go too.
        let secret = "a\"b c/d".to_owned();
        let said = format!("{} and {} and {}", secret, "a\\\"b c/d", "a%22b%20c%2Fd");
        assert_eq!(scrub(&said, &[secret]), "••• and ••• and •••");
    }

    #[test]
    fn only_the_enabled_browser_stdio_server_is_asked() {
        let d = tempfile::tempdir().unwrap();
        let st = crate::config::tests::store(d.path());
        // An unknown id, a server that is not the browser preset and a turned-off one all
        // answer without spawning anything.
        assert_eq!(run(&st, "nope").message, "That server no longer exists.");
        st.mutate(None, |l| {
            l.push(crate::config::tests::browser_server("bb"));
            l.push(crate::config::tests::browser_server("off"));
            if let Some(o) = l.iter_mut().find(|s| s.id == "off") {
                o.enabled = false;
            }
            Ok(())
        })
        .unwrap();
        let a = run(&st, "other");
        assert!(!a.ok);
        st.mutate(None, |l| {
            if let Some(o) = l.iter_mut().find(|s| s.id == "bb") {
                o.preset = None;
            }
            Ok(())
        })
        .unwrap();
        assert!(!run(&st, "bb").ok, "not the browser preset: not asked");
        st.mutate(None, |l| {
            l.iter_mut().find(|s| s.id == "bb").unwrap().preset = Some(BROKER_PRESET_ID.into());
            Ok(())
        })
        .unwrap();
        // A preset server whose arguments name no broker entry is refused without spawning.
        st.mutate(None, |l| {
            l.iter_mut().find(|s| s.id == "bb").unwrap().transport.args = vec!["thing.js".into()];
            Ok(())
        })
        .unwrap();
        let e = run(&st, "bb");
        assert!(!e.ok);
        assert!(e.message.contains("broker-tool"));
        assert!(!run(&st, "off").ok, "turned off: not asked");
    }

    #[test]
    fn a_real_run_returns_the_brokers_checks() {
        if Command::new("node").arg("--version").output().is_err() {
            return; // same guard as the probe's node tests
        }
        let d = tempfile::tempdir().unwrap();
        // A script that plays the broker's doctor: prints a report like the real one to stdout.
        let script_dir = d.path().join("dist").join("src").join("bin");
        std::fs::create_dir_all(&script_dir).unwrap();
        std::fs::write(
            script_dir.join("broker.js"),
            r#"console.log(JSON.stringify({checks:[{id:"attached.token",title:"The token is set",status:"ok",detail:"set (from " + (process.env.BROKER_ATTACHED_TOKEN ?? "") + ")"},{id:"attached.binary",title:"The browser is installed",status:"failed",detail:"missing",remedy:"install it"}],exitCode:1}));"#,
        )
        .unwrap();
        std::fs::write(
            script_dir.join("broker-tool.js"),
            "// never run by the doctor path",
        )
        .unwrap();
        let st = crate::config::tests::store(d.path());
        st.mutate(None, |l| {
            let mut s = crate::config::tests::browser_server("bb");
            s.transport.command = Some("node".into());
            s.transport.args = vec![script_dir
                .join("broker-tool.js")
                .to_string_lossy()
                .into_owned()];
            s.transport.env.insert(
                "BROKER_ATTACHED_TOKEN".into(),
                crate::types::ValueRef {
                    kind: crate::types::RefKind::Literal,
                    value: Some("tok_secret".into()),
                    secret_id: None,
                    prefix: None,
                },
            );
            l.push(s);
            Ok(())
        })
        .unwrap();
        let out = run(&st, "bb");
        assert!(!out.ok, "the fake report has a failed check");
        assert_eq!(out.message, "1 of 2 checks did not pass");
        assert_eq!(out.checks.len(), 2);
        assert_eq!(out.checks[1].remedy.as_deref(), Some("install it"));
        // The token the doctor saw is scrubbed out of every string that reaches the screen.
        let text = serde_json::to_string(&out).unwrap();
        assert!(
            !text.contains("tok_secret"),
            "no resolved secret in the answer"
        );
    }
}
