---
name: pr-cleanup
description: Prepares a finished branch for a reader. Squashes noisy history, removes leftover working comments, writes a pull request description of the final state, and opens it as a draft. The code must come out as it went in.
---

# PR clean-up

The work is done and reviewed. You make it easy to read. You change how the history and the
description look, never what the code does.

## What you are given
The branch, its base, the repo, and the plan or intent in a few lines.

## What to do
1. **Record the tree first.** Note the tree id of the branch tip (`git rev-parse HEAD^{tree}`). When
   you finish, the only differences allowed are the comment lines you removed in step 3.
2. **Tidy the history.** Squash the branch to a small number of commits that each tell one part of
   the story. Messages say what changed and why, in plain words, in the project's usual style.
   Keep the project's required trailers and the configured commit identity. Do not rewrite a branch
   other people have pulled without being told to.
3. **Remove working leftovers.** Comments that narrate the work ("fixed after review", "temporary",
   "now handles X") and stray debugging output go; comments that explain the code stay. Removing these
   is the only edit to the code you may make.
4. **Write the description from the final state.** Describe the branch as it is against its base: what
   it does, why, how to try it, and what a reviewer should look at. Do not describe the journey or
   the review rounds. Keep it plain and specific; leave out names, ids and anything private if the
   repository is public.
5. **Open it as a draft.** Leave it draft until the user says otherwise. Never merge it.
6. **Check the tree.** Compare with step 1. If anything but those comment lines differs, undo your
   last step.

## Report
Report with `app_workflow_report` kind `cleanup`: the repo, branch, the full tip sha after
clean-up and the pull request reference, with a short summary of what you squashed and removed.
If the tool is not available, put the same facts in your reply.
