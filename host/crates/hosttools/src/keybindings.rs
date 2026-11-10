//! The checks a shortcut gets when a chat sets it, the same two the Settings page applies when the
//! user rebinds one: the chord is a real chord, and it is not one Windows or the web view keeps
//! for itself. Two commands on one chord are refused too.
//!
//! Defaults are not known here, so a chord that clashes with a command the chat did not mention
//! is caught by the interface, which reports every clash it finds and lets the user's own binding
//! win. Invisible and direction-changing characters are screened before this runs.

use serde_json::Value;

/// Chords Windows or the web view own. Every chord with Super (the Windows key) is reserved too.
const RESERVED: &[&str] = &[
    "Alt+F4",
    "Alt+Tab",
    "Alt+Space",
    "Ctrl+Alt+Delete",
    "Ctrl+Escape",
    "Ctrl+Shift+Escape",
    "Ctrl+Shift+I",
    "Ctrl+Shift+J",
    "F12",
];

/// The chord in its one spelling (`Ctrl+Alt+Shift+Super+Key`), or `None` when it is not a chord:
/// no key, or an unknown modifier.
pub(crate) fn normalise(chord: &str) -> Option<String> {
    let mut parts: Vec<&str> = chord
        .split('+')
        .map(str::trim)
        .filter(|p| !p.is_empty())
        .collect();
    let key = parts.pop()?;
    let (mut ctrl, mut alt, mut shift, mut sup) = (false, false, false, false);
    for p in parts {
        match p.to_lowercase().as_str() {
            "ctrl" | "control" | "mod" | "cmd" | "cmdorctrl" => ctrl = true,
            "alt" | "option" | "opt" => alt = true,
            "shift" => shift = true,
            "super" | "win" => sup = true,
            _ => return None,
        }
    }
    let key = if key.chars().count() == 1 {
        key.to_uppercase()
    } else {
        let mut cs = key.chars();
        let first = cs.next()?;
        first.to_uppercase().chain(cs).collect()
    };
    let mut out: Vec<&str> = Vec::new();
    for (on, name) in [
        (ctrl, "Ctrl"),
        (alt, "Alt"),
        (shift, "Shift"),
        (sup, "Super"),
    ] {
        if on {
            out.push(name);
        }
    }
    let mods = out.join("+");
    Some(if mods.is_empty() {
        key
    } else {
        format!("{mods}+{key}")
    })
}

fn reserved(chord: &str) -> bool {
    RESERVED.contains(&chord) || chord.split('+').rev().skip(1).any(|m| m == "Super")
}

/// Check a whole `keybindings` value (`{commandId: chord or null}`).
pub(crate) fn check(v: &Value) -> Result<(), String> {
    let Some(map) = v.as_object() else {
        return Ok(()); // the shape is checked elsewhere
    };
    let mut taken: Vec<(String, &str)> = Vec::new();
    for (id, chord) in map {
        let Some(chord) = chord.as_str() else {
            continue; // null clears a binding
        };
        let n = normalise(chord).ok_or_else(|| format!("\"{chord}\" is not a shortcut."))?;
        if reserved(&n) {
            return Err(format!(
                "{n} belongs to Windows or the web view and cannot be used."
            ));
        }
        if let Some((_, other)) = taken.iter().find(|(c, _)| *c == n) {
            return Err(format!("{n} is given to both \"{other}\" and \"{id}\"."));
        }
        taken.push((n, id.as_str()));
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    #[test]
    fn chords_are_normalised_the_way_the_interface_does() {
        for (raw, want) in [
            ("ctrl+k", Some("Ctrl+K")),
            ("Mod+Shift+p", Some("Ctrl+Shift+P")),
            ("shift+alt+control+x", Some("Ctrl+Alt+Shift+X")),
            ("win+e", Some("Super+E")),
            ("escape", Some("Escape")),
            ("f5", Some("F5")),
            ("ctrl+", Some("Ctrl")),
            ("hyper+k", None),
            ("", None),
            ("+", None),
        ] {
            assert_eq!(normalise(raw).as_deref(), want, "{raw}");
        }
    }

    #[test]
    fn reserved_unknown_and_doubled_chords_are_refused() {
        assert!(check(&json!({"a": "Ctrl+K", "b": "ctrl+shift+p", "c": null})).is_ok());
        assert!(check(&json!([])).is_ok(), "shape is checked elsewhere");
        for (bad, says) in [
            (json!({"a": "alt+f4"}), "belongs to Windows"),
            (json!({"a": "F12"}), "belongs to Windows"),
            (json!({"a": "Win+L"}), "belongs to Windows"),
            (json!({"a": "Ctrl+Super+X"}), "belongs to Windows"),
            (json!({"a": "hyper+k"}), "is not a shortcut"),
            (json!({"a": "Ctrl+K", "b": "mod+k"}), "given to both"),
        ] {
            let why = check(&bad).unwrap_err();
            assert!(why.contains(says), "{bad}: {why}");
        }
    }
}
