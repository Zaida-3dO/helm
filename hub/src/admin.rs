//! The admin commands, run on the server with `docker exec`: people, machines and pairing codes.
//! They work on the registry file directly, so a revoke needs no restart.
//!
//! ```text
//! helm-hub person add <name>
//! helm-hub person list
//! helm-hub person merge <from> <into>
//! helm-hub client add --person P --machine M [--scopes S] [--acts-for A]   prints the token once
//! helm-hub client list
//! helm-hub client revoke <machine or id> [--person P]
//! helm-hub client acts-for <machine or id> <person> [--person P]
//! helm-hub client stop-acting-for <machine or id> <person> [--person P]
//! helm-hub pair --person P --machine M [--scopes S] [--acts-for A]         prints a one-time code
//! ```

use crate::auth::{Scopes, DEFAULT_PAIRED_SCOPES};
use crate::registry::{display_code, ClientRow, Registry, PAIR_CODE_MINUTES};
use std::collections::HashMap;

pub const USAGE: &str = "  helm-hub person add <name> | person list
  helm-hub person merge <from> <into>
                                       give everything of one person (machines, memories,
                                       rules, skills and agents) to another, and remove the first
  helm-hub client add --person P --machine M [--scopes S] [--acts-for A]
                                       register a machine and print its token (once)
  helm-hub client list
  helm-hub client revoke <machine or id> [--person P]
                                       the machine's token stops working at once
  helm-hub client acts-for <machine or id> <person> [--person P]
  helm-hub client stop-acting-for <machine or id> <person> [--person P]
                                       let a machine act for another person (read their private
                                       memories, change their rules, skills and agents), or stop it
  helm-hub pair --person P --machine M [--scopes S] [--acts-for A]
                                       print a one-time pairing code (8 characters, 15 minutes)

  Scopes: memory:ro, memory:rw, relay, app, peer, telegram, voice, remote (comma separated;
  telegram, voice and remote stand alone).
  Default for a machine: memory:rw,relay,app,peer. A person that does not exist yet is made.
  --acts-for takes people who exist, comma separated; a machine always acts for its own person.";

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
        ["person", "merge", rest @ ..] => {
            let (pos, _) = flags(rest, &[])?;
            let [from, into] = pos.as_slice() else {
                return Err(
                    "person merge takes two people: the one to fold in, then the one to keep"
                        .into(),
                );
            };
            let m = registry
                .merge_person(from, into)
                .map_err(|e| e.to_string())?;
            Ok(format!(
                "merged {} into {}: {} machine(s), {} memories, {} rule, skill and agent file(s); \
                 {} is gone. If {} was HUB_LEGACY_PERSON, set it to {} and restart the hub.\n",
                m.from.name,
                m.into.name,
                m.machines,
                m.memories,
                m.files,
                m.from.name,
                m.from.name,
                m.into.name
            ))
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
            let (_, f) = flags(rest, &["person", "machine", "scopes", "acts-for"])?;
            let (person, machine) = required(&f)?;
            let scopes = scopes_of(&f)?;
            let others = acts_for_of(&f);
            for who in &others {
                registry.find_person(who).map_err(|e| e.to_string())?;
            }
            let p = registry
                .add_client(person, machine, &scopes)
                .map_err(|e| e.to_string())?;
            for who in &others {
                registry
                    .set_acts_for(p.client.id, who, true)
                    .map_err(|e| e.to_string())?;
            }
            Ok(format!(
                "added {} for {} (id {}, {})\nToken, shown once (copy it now): {}\n",
                p.client.machine,
                p.client.person,
                p.client.id,
                p.client.scopes.to_csv(),
                p.token
            ))
        }
        ["client", verb @ ("acts-for" | "stop-acting-for"), rest @ ..] => {
            let (pos, f) = flags(rest, &["person"])?;
            let [who, other] = pos.as_slice() else {
                return Err(format!(
                    "client {verb} takes a machine name or id, then a person"
                ));
            };
            let c = registry
                .find_client(who, f.get("person").map(String::as_str))
                .map_err(|e| e.to_string())?;
            let allow = *verb == "acts-for";
            let p = registry
                .set_acts_for(c.id, other, allow)
                .map_err(|e| e.to_string())?;
            Ok(if allow {
                format!(
                    "{} of {} now acts for {} too\n",
                    c.machine, c.person, p.name
                )
            } else {
                format!(
                    "{} of {} no longer acts for {}\n",
                    c.machine, c.person, p.name
                )
            })
        }
        ["client", "list"] => {
            let clients = registry.clients().map_err(|e| e.to_string())?;
            let mut out = String::new();
            for c in &clients {
                out.push_str(&describe(c));
                let others = registry.acts_for(c.id).map_err(|e| e.to_string())?;
                if !others.is_empty() {
                    out.push_str(&format!("  acts for {}", others.join(", ")));
                }
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
            let (_, f) = flags(rest, &["person", "machine", "scopes", "acts-for"])?;
            let (person, machine) = required(&f)?;
            let scopes = scopes_of(&f)?;
            let others = acts_for_of(&f);
            let others: Vec<&str> = others.iter().map(String::as_str).collect();
            let (code, _) = registry
                .create_pairing_acting(person, machine, &scopes, &others, now_ms)
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

/// The people of `--acts-for`, comma separated.
fn acts_for_of(f: &HashMap<String, String>) -> Vec<String> {
    f.get("acts-for")
        .map(|v| {
            v.split(',')
                .map(str::trim)
                .filter(|w| !w.is_empty())
                .map(str::to_owned)
                .collect()
        })
        .unwrap_or_default()
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
        // A machine may be allowed to act for another person who exists, and stopped again.
        assert!(run_line(
            &mut r,
            "client add --person Sam --machine pc --acts-for Nobody"
        )
        .is_err());
        run_line(
            &mut r,
            "client add --person Sam --machine pc --acts-for Kit",
        )
        .unwrap();
        let pc = r.find_client("pc", Some("Sam")).unwrap().id;
        let kit = r.find_person("Kit").unwrap().id;
        assert!(r.may_act_for(pc, kit));
        assert!(run_line(&mut r, "client list").unwrap().contains(
            "Sam / pc  [memory:rw,relay,app,peer]  active  last seen never  acts for Kit"
        ));
        assert!(run_line(&mut r, "client stop-acting-for pc Kit")
            .unwrap()
            .contains("no longer acts for Kit"));
        assert!(!r.may_act_for(pc, kit));
        run_line(&mut r, "client acts-for pc Kit --person Sam").unwrap();
        assert!(r.may_act_for(pc, kit));
        // A code made with --acts-for gives the new machine the same allowance.
        let code = run_line(&mut r, "pair --person Kit --machine tablet --acts-for Sam").unwrap();
        let code = code.split(": ").nth(1).unwrap().split('\n').next().unwrap();
        let tablet = r.redeem_pairing(code, 1_000_001).unwrap().client.id;
        let sam = r.find_person("Sam").unwrap().id;
        assert!(r.may_act_for(tablet, sam) && r.may_act_for(tablet, kit));

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
