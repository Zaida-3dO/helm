---
name: orchestrate
description: How to run a piece of work as an orchestrator with the planner, coder, reviewer and pr-cleanup agents: one plan, builders that only write code on one feature branch, one review, one fix pass, one pull request. Load it when asked to orchestrate, split work into parts, or hand work to several agents.
---

# Orchestrating a piece of work

One piece of work is one branch and one pull request. The agents do the stages; you coordinate and
build nothing yourself.

## The stages
1. **Plan.** Have the `planner` write one plan to a file, with the work split into parts that each
   own their own files. If the user's request is small, the plan can be a paragraph and the "split"
   a single part. For risky or wide work, have the `plan-reviewer` read the plan once before anyone
   builds.
2. **Branch.** Make one feature branch from the project's main line, named for the work.
3. **Build.** For each part, one `coder`: its own worktree and its own branch off the feature branch,
   the plan path, its part, and a clear line on whether it may build and test or only write code.
   Parts that touch different files can run side by side. When a coder finishes, merge its branch
   into the feature branch.
4. **Review once.** Start the `reviewer` on the finished feature branch, on the strongest model
   available. It builds, runs the checks, reads the whole diff and writes one fix list.
5. **Fix once.** One `coder` applies the whole list. It does not go back to the reviewer.
6. **Clean up and open the pull request.** The `pr-cleanup` agent tidies the history, writes the
   description and opens a draft. Merging is the user's call.

Add the `smoketester` after the review when the change has a side a person will see.

## Rules that keep it cheap and clear
- No review loop. One review, one fix pass. Open follow-ups for what is left over.
- Hand work over in files, not long messages: `plan`, a hand-off note per part, the review's fix
  list. Read each agent's short report, not everything it touched.
- A brief is self-contained: paths, scope, branch, worktree, what to report, and what it must not
  touch. An agent should never need the conversation.
- Long stages (a build, a review) run as their own chat or background agent, with the report
  coming back by message and a wait as backstop. End your turn rather than sit and poll.
- Check that a report really arrived. A missing report is a reason to read the agent's output,
  not to assume it succeeded.
- Check usage before the expensive stage. If the allowance is nearly gone, stop and ask.
- Never merge, release or push to the main line without the user's say-so. Never put secrets or
  private details in commits, descriptions or repository files.

## After the run
If something went wrong in a way a better brief or a different recipe would have prevented, write a
lesson with the `run-lessons` skill before you move on.
