---
name: learn-from-pr
description: Turn a problem a review missed, found later in a pull request or in use, into one general rule for the reviewer, or a reasoned decision not to add one. Load it when a change shipped with a defect the review should have caught, or when asked to improve the reviewer's rules.
---

# Learning from a miss

When a defect gets past the review, ask what a better reviewer would have looked for. The answer is
a rule the reviewer reads next time, kept in a short file of staged rules beside its agent file
(`reviewer-rules.md`; create it if it is not there).

## Decide first
1. **What was missed?** Describe the defect and where it was in a sentence. Show the code that
   would have caught it on a read.
2. **Is it general?** A rule must apply to future changes, not just this one. "Check that every
   place that builds a file path also handles a missing folder" is general. "Check the settings
   page" is not.
3. **Does a rule already cover it?** If so, the rule is unclear or ignored; sharpen it rather than
   adding another.
4. **Would the rule have fired?** Imagine the reviewer with the rule and this diff. If it would
   still have walked past the defect, the rule is wrong.

If the miss was a one-off, or no rule would have helped, write that down as a rejection with the
reason, so it is not proposed again.

## The file
```
## Rules
- R-001 (seen 3x, since <date>): <one sentence, imperative, general>
  Why: <the kind of defect it prevents>
  How to check: <what to look at in a diff>

## Considered and rejected
- <the idea> — <why it was not added>
```

## Rules for the rules
- A staged rule starts at medium severity at most. A reviewer reads staged rules as guidance and
  still uses its own judgement; it does not follow them blindly.
- Count how often each rule catches something or would have. A rule seen three times is worth
  promoting into the reviewer's own recipe. A rule that has not fired in many runs, or that only
  produces noise, is retired: move it to "Considered and rejected" with the reason.
- Keep the whole file short enough to read in a minute.
