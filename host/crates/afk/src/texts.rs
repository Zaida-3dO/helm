//! The words AFK says to agents: the notes when it starts and ends, the line every turn carries
//! while it is on, and the automatic answer to a question. Only the answer is the user's to edit;
//! the safety clause is always last and cannot be edited away.

use crate::digest::hhmm;

/// Appended to every automatic answer.
pub const SAFETY: &str = "This reply approves nothing irreversible or outward-facing: no deleting or overwriting, no force-pushes, no merges or deployments, no publishing, no posting messages or comments, no paying for anything, and no changes to credentials or permissions. If a step like that is needed, the answer is \"no for now\": do not get the same result another way, and list it under \"Needs you\" for when the user is back.";

/// The built-in answer to a question, used when the user has not written one.
pub const DEFAULT_ANSWER: &str = "The user is away and cannot answer. Pick the most reasonable default and carry on, or hand the hard choice to another agent. If you cannot choose safely, set the question aside, keep doing work that is not blocked by it, and list it under \"Needs you\".";

/// The automatic answer: the user's text (or the built-in one) and then the safety clause.
pub fn answer(custom: Option<&str>) -> String {
    let body = custom
        .map(str::trim)
        .filter(|s| !s.is_empty())
        .unwrap_or(DEFAULT_ANSWER);
    format!("{body}\n\n{SAFETY}")
}

fn what_changes(plan_approve: bool) -> String {
    let plan = if plan_approve {
        "A plan you propose for approval is approved automatically."
    } else {
        "A plan you propose for approval waits for them: stay in plan mode and do not ask again until they are back."
    };
    format!(
        "Questions you ask are answered automatically. Permission cards and Allow requests are not answered: they wait for the user, so do not retry them, do not get the same effect another way, and list them under \"Needs you\". {plan}"
    )
}

/// The note a working chat gets when AFK turns on.
pub fn on_note(since: i64, plan_approve: bool) -> String {
    format!(
        "[Helm · AFK is on] The user has been away since {}. {} Keep doing the work that is not blocked, and write up anything that needs them.",
        hhmm(since),
        what_changes(plan_approve)
    )
}

/// The note a working chat gets when AFK ends.
pub fn off_note() -> String {
    "[Helm · AFK is off] The user is back. Questions go to them again, and any answers given for them while they were away were stand-ins they may want to revisit.".to_owned()
}

/// What every new turn carries while AFK is on.
pub fn turn_text(since: i64, plan_approve: bool) -> String {
    format!(
        "[Helm · AFK] The user is away (since {}). {}",
        hhmm(since),
        what_changes(plan_approve)
    )
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn answers_end_with_the_safety_clause() {
        assert!(answer(None).starts_with(DEFAULT_ANSWER));
        assert!(answer(Some("  ")).starts_with(DEFAULT_ANSWER));
        let a = answer(Some("Use the safe option."));
        assert!(a.starts_with("Use the safe option.") && a.ends_with(SAFETY));
        assert!(turn_text(0, true).contains("approved automatically"));
        assert!(on_note(0, false).contains("waits for them"));
    }
}
