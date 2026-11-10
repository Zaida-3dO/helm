//! The admin commands, run on the server with `docker exec`: people, machines and pairing codes.
//! They work on the registry file directly, so a revoke needs no restart.
//!
//! ```text
//! helm-hub person add <name>
//! helm-hub person list
//! helm-hub client add --person P --machine M [--scopes S]   prints the token once
//! helm-hub client list
//! helm-hub client revoke <machine or id> [--person P]
//! helm-hub pair --person P --machine M [--scopes S]         prints a one-time code
//! ```

use crate::auth::{Scopes, DEFAULT_PAIRED_SCOPES};
use crate::registry::{display_code, ClientRow, Registry, PAIR_CODE_MINUTES};
use std::collections::HashMap;

pub const USAGE: &str = "  helm-hub person add <name> | person list
  helm-hub client add --person P --machine M [--scopes S]
                                       register a machine and print its token (once)
  helm-hub client list
  helm-hub client revoke <machine or id> [--person P]
                                       the machine's token stops working at once
  helm-hub pair --person P --machine M [--scopes S]
                                       print a one-time pairing code (8 characters, 15 minutes)

  Scopes: memory:ro, memory:rw, relay, app, peer (comma separated).
  Default for a machine: memory:rw,relay,app,peer. A person that does not exist yet is made.";

/// Whether `first` begins an admin command.
pub fn is_admin_command(first: &str) -> bool {
    matches!(first, "person" | "client" | "pair")
}

/// Run one admin command and return what to print.
pub fn run(registry: &mut Registry, args: &[String], now_ms: i64) -> Result<String, String> {
    let strs: Vec<&str> = args.iter().map(String::as_str).collect();
    match strs.as_slice() {
        ["person", "add", rest @ ..] => {
            let (pos, _) = flags(rest, &[])?;
            let [name] = pos.as_slice() else {
                return Err("person add takes one name".into());
            };
            let before = registry.person_by_name(name).map_err(|e| e.to_string())?;
            let p = registry.ensure_person(name).map_err(|e| e.to_string())?;
            Ok(match before {
                Some(_) => format!("{} already exists (id {})", p.name, p.id),
                None => format!("added {} (id {})", p.name, p.id),
            })
        }
        ["person", "list"] => {
            let people = registry.people().map_err(|e| e.to_string())?;
            let clients = registry.clients().map_err(|e| e.to_string())?;
            let mut out = String::new();
            for p in &people {
                let n = clients
                    .iter()
                    .filter(|c| c.person_id == p.id && c.revoked_at.is_none())
                    .count();
                out.push_str(&format!("{:>4}  {}  ({n} machine(s))\n", p.id, p.name));
            }
            if people.is_empty() {
                out.push_str("no people yet\n");
            }
            Ok(out)
        }
        ["client", "add", rest @ ..] => {
            let (_, f) = flags(rest, &["person", "machine", "scopes"])?;
            let (person, machine) = required(&f)?;
            let scopes = scopes_of(&f)?;
            let p = registry
                .add_client(person, machine, &scopes)
                .map_err(|e| e.to_string())?;
            Ok(format!(
                "added {} for {} (id {}, {})\nToken, shown once (copy it now): {}\n",
                p.client.machine,
                p.client.person,
                p.client.id,
                p.client.scopes.to_csv(),
                p.token
            ))
        }
        ["client", "list"] => {
            let clients = registry.clients().map_err(|e| e.to_string())?;
            let mut out = String::new();
            for c in &clients {
                out.push_str(&describe(c));
                out.push('\n');
            }
            if clients.is_empty() {
                out.push_str("no clients yet\n");
            }
            Ok(out)
        }
        ["client", "revoke", rest @ ..] => {
            let (pos, f) = flags(rest, &["person"])?;
            let [who] = pos.as_slice() else {
                return Err("client revoke takes one machine name or id".into());
            };
            let c = registry
                .revoke(who, f.get("person").map(String::as_str))
                .map_err(|e| e.to_string())?;
            Ok(format!(
                "revoked {} of {}; its token stopped working and an open line drops within seconds",
                c.machine, c.person
            ))
        }
        ["pair", rest @ ..] => {
            let (_, f) = flags(rest, &["person", "machine", "scopes"])?;
            let (person, machine) = required(&f)?;
            let scopes = scopes_of(&f)?;
            let (code, _) = registry
                .create_pairing(person, machine, &scopes, now_ms)
                .map_err(|e| e.to_string())?;
            Ok(format!(
                "Pairing code for {machine} ({person}): {}\nIt works once, for {PAIR_CODE_MINUTES} minutes. \
                 In Helm: Settings → Hub → Pair with a code.\n",
                display_code(&code)
            ))
        }
        _ => Err("not an admin command".into()),
    }
}

fn describe(c: &ClientRow) -> String {
    let state = if c.revoked_at.is_some() {
        "revoked"
    } else if c.legacy {
        "active, from HUB_TOKENS"
    } else {
        "active"
    };
    format!(
        "{:>4}  {} / {}  [{}]  {state}  last seen {}",
        c.id,
        c.person,
        c.machine,
        c.scopes.to_csv(),
        c.last_seen_at.as_deref().unwrap_or("never")
    )
}

fn required(f: &HashMap<String, String>) -> Result<(&str, &str), String> {
    match (f.get("person"), f.get("machine")) {
        (Some(p), Some(m)) => Ok((p.as_str(), m.as_str())),
        _ => Err("--person and --machine are both required".into()),
    }
}

fn scopes_of(f: &HashMap<String, String>) -> Result<Scopes, String> {
    Scopes::parse_list(
        f.get("scopes")
            .map(String::as_str)
            .unwrap_or(DEFAULT_PAIRED_SCOPES),
    )
}

/// Split `args` into positional words and `--name value` / `--name=value` flags, refusing any
/// flag not in `allowed`.
fn flags(
    args: &[&str],
    allowed: &[&str],
) -> Result<(Vec<String>, HashMap<String, String>), String> {
    let (mut pos, mut found) = (Vec::new(), HashMap::new());
    let mut it = args.iter();
    while let Some(a) = it.next() {
        let Some(flag) = a.strip_prefix("--") else {
            pos.push((*a).to_owned());
            continue;
        };
        let (name, value) = match flag.split_once('=') {
            Some((n, v)) => (n, v.to_owned()),
            None => (
                flag,
                it.next()
                    .map(|v| (*v).to_owned())
                    .ok_or_else(|| format!("--{flag} needs a value"))?,
            ),
        };
        if !allowed.contains(&name) {
            return Err(format!("unknown option --{name}"));
        }
        if found.insert(name.to_owned(), value).is_some() {
            return Err(format!("--{name} given twice"));
        }
    }
    Ok((pos, found))
}

#[cfg(test)]
mod tests {
    use super::*;

    fn run_line(r: &mut Registry, line: &str) -> Result<String, String> {
        let args: Vec<String> = line.split_whitespace().map(str::to_owned).collect();
        run(r, &args, 1_000_000)
    }

    #[test]
    fn the_commands_make_people_machines_codes_and_revokes() {
        let mut r = Registry::open_in_memory().unwrap();
        assert!(run_line(&mut r, "person add Sam")
            .unwrap()
            .starts_with("added Sam"));
        assert!(run_line(&mut r, "person add sam")
            .unwrap()
            .contains("already exists"));
        let added = run_line(
            &mut r,
            "client add --person Sam --machine desk --scopes memory:ro,app",
        )
        .unwrap();
        let token = added
            .split("copy it now): ")
            .nth(1)
            .unwrap()
            .trim()
            .to_owned();
        let c = r.authenticate(Some(&format!("Bearer {token}"))).unwrap();
        assert_eq!(c.scopes.to_csv(), "memory:ro,app");
        // A pairing for a person who does not exist yet makes them; the code is in the output.
        let paired = run_line(&mut r, "pair --person=Kit --machine laptop").unwrap();
        assert!(
            paired.contains("Pairing code for laptop (Kit): "),
            "{paired}"
        );
        assert!(run_line(&mut r, "person list")
            .unwrap()
            .contains("Kit  (0 machine(s))"));
        let listed = run_line(&mut r, "client list").unwrap();
        assert!(
            listed.contains("Sam / desk") && listed.contains("active"),
            "{listed}"
        );
        // Revoke by name; the token dies at once.
        assert!(run_line(&mut r, "client revoke desk")
            .unwrap()
            .contains("revoked desk of Sam"));
        assert!(r.authenticate(Some(&format!("Bearer {token}"))).is_none());
        assert!(run_line(&mut r, "client revoke desk").is_err());
        // Bad input.
        for bad in [
            "pair --person Sam",
            "pair --person Sam --machine m --scopes admin",
            "client add --person Sam --machine m --bogus 1",
            "client revoke",
            "pair --person Sam --machine",
        ] {
            assert!(run_line(&mut r, bad).is_err(), "{bad}");
        }
    }
}
