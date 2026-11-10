---
name: run-lessons
description: Keep a short log of what went wrong in an orchestrated run and which instruction should change because of it. Load it at the end of a run that went badly, or when asked to record or review lessons.
---

# Run lessons

A lesson is written for the next run, not as a diary. Keep one file of them, `run-lessons.md`, in
the folder where the project keeps its working notes (create it if it is not there). Read it at the
start of a run, before you brief anyone.

## When to write one
Only when a run went wrong in a way that more careful instructions would have prevented: an agent
did the wrong work, a stage was skipped, a report never arrived, a reviewer missed something a
later check caught, time or allowance was burned on a loop. A run that went well needs no entry.

## One entry per run
```
## <date> — <the work, in a few words>
- What happened: <one or two sentences, facts only>
- Role that failed: <planner | coder | reviewer | orchestrator | the brief | the tooling>
- Why: <the missing instruction, wrong assumption or unclear scope>
- Change: <the exact line to add or change in which agent file or skill>
```

## Rules
- Blame the instruction, not the agent. If a role failed, the question is what its recipe lacked.
- Make the change. An entry whose "Change" has not been made is not finished: edit the agent or
  skill, then mark the entry done.
- If the same role fails the same way twice, the first fix was not enough. Say so in the new entry.
- Keep it short. Merge entries that say the same thing and delete ones that no longer apply.
