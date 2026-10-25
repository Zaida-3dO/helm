//! What an orchestrator does when writes to the task board fail.
//!
//! The board is never a gate: plans, verdicts, approvals, hand-offs and landing live in Helm's own
//! records, so work goes on while the board is down. Only the agent's own board writes need a
//! rule, and nothing in the app enforces it: it is practice the orchestrator follows and passes
//! to the agents it dispatches. Stage records that Helm writes itself are queued by
//! [`crate::sa_outbox`] and retried every minute; this text is about the agent's writes.

/// The rule appended to the generated Workflow rules. Fixed text; it grants nothing.
pub const BOARD_OUTAGE: &str = "If a write to the task board fails, keep working and do not stop the task for it. First read what the board said: a failure that says it did not commit and may be retried can be tried again a few seconds later, at most twice. Any other failure (a timeout, a lost reply, an unknown outcome) may have gone through, so read the item again before anything else and never repeat the write blind. Treat the board as down only when both ways of reaching it fail. While it is down, keep one pending-records file for this chat, outside every repository (for example in the system temporary folder, named helm-pending-board-records.md): one line per write still owed, with the item, what kind of record, the verdict, the full commit, the pull request link and the time. When the board answers again, write the lines in order, claiming the item first; if the claim is refused because another crew holds it, stop and tell the user before recording anything else. Add \"recorded\" and the time to each line as it goes through, and delete the file when every line is recorded. Tell every agent you dispatch the same: a reviewer who cannot record a verdict returns it in the chat as a fenced review record, says the record could not be written, never claims a record it did not write and never lowers its verdict; you write that record to the board yourself when it returns, marked as transcribed by you, and a transcribed record never counts as the last reviewed verdict, so the next review of that pull request is a full one. A clean-up agent goes ahead only if its brief quotes the reviewer's approving verdict, and lists its own unwritten board records in its report. A crew with no board access puts its progress in its report.";

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_rule_covers_each_step_of_an_outage() {
        for part in [
            "did not commit and may be retried",
            "read the item again",
            "pending-records file",
            "outside every repository",
            "in order, claiming the item first",
            "fenced review record",
            "never counts as the last reviewed verdict",
        ] {
            assert!(BOARD_OUTAGE.contains(part), "missing: {part}");
        }
    }
}
