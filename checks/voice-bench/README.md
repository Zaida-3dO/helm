# Voice chat benchmark

Helps pick the voice chat's model. It plays 20 synthetic prompts (`prompts.ts`) into the voice chat
once per candidate: Haiku 4.5, Sonnet 5.5 on low, Sonnet 5.5 on medium and, if you set it up, a
Strata model. For each it records the time to the answer, what the spoken-answer cleanup had to take
out, how long the answers are, and how often the chat stays quiet about news. It writes a table and
every reply, for you to listen through and score.

**It spends plan usage** (real `claude` under your own sign-in, about 20 short turns per candidate).
It never runs from `npm test` or `npm run e2e`, and without `HELM_LIVE_CLAUDE=1` it only prints its
plan. It contacts no model server other than the ones you name below.

## Run it

```
npm run build:debug                                   # the runner drives target/debug/helm.exe
cargo build -p helm-relay --examples                  # the cleanup counter (builds, contacts nothing)
HELM_LIVE_CLAUDE=1 npm run voice-bench -w checks -- --quick     # seven prompts, a cheap trial
HELM_LIVE_CLAUDE=1 npm run voice-bench -w checks                # all twenty, every candidate
```

Options (after `--`): `--only haiku,sonnet-low` to pick candidates, `--timeout <seconds>` for one
answer (default 240), `--out <folder>`. Results go to `checks/voice-bench/out/<time>/` (not
committed): `results.md` (the table), `results.json`, and `replies-<candidate>.md` with every
prompt and reply and a `Score:` line to fill in. `--recount <results folder>` redoes just the
cleanup counts of an earlier run; it starts nothing and spends nothing.

The app runs on a scratch profile, windowless in the background like the end-to-end checks, so
your installed Helm's profile is never touched. A prompt that makes the chat ask for an approval is
recorded as "waiting on an approval": voice never answers approvals. The prompts carry every fact
they need so that rarely happens.

## How it measures

The runner is a fake hub: Helm's debug build connects its relay line to it, and the runner sends
the same `voice` frames the phone's turns arrive as. "Time to the answer" is from sending the turn
to its `voice_final` arriving, so on the phone add speech-to-text and the network (measure those
once on the device). News prompts arrive as a message from another chat, which is how the voice
chat is woken for things it was not asked about; the table shows how often it told you and how
often it answered exactly `[quiet]`, against what each prompt wanted.

## The Strata candidate

Off unless all three are set. Helm's debug build then sends that candidate's chat to your local
model server, and only that one:

- `HELM_BENCH_STRATA_MODEL`: the model id as Helm's model picker shows it for that server.
- `HELM_BENCH_STRATA_ADDR`: the server's loopback address, like `http://127.0.0.1:8080`.
- `HELM_BENCH_STRATA_PROFILE`: a scratch **copy** of a Helm profile folder in which that server is
  already set up in Settings (the runner does not set one up).

Model ids for the Claude candidates can be changed with `HELM_BENCH_HAIKU_MODEL` (default `haiku`)
and `HELM_BENCH_SONNET_MODEL` (default `claude-sonnet-5-5`).

## Files

- `prompts.ts`: the 20 prompts and which seven make up `--quick`.
- `fake-hub.ts`: the fake hub line (a hand-written WebSocket server on loopback, no dependencies).
- `report.ts`: the table and the per-candidate replies document (pure functions).
- `run.ts`: the runner.
- `host/crates/relay/examples/speakable_counts.rs`: runs the real cleanup over the collected answers.
- Tests: `checks/e2e/voice-bench.e2e.ts` (the prompts, the fake hub and the table maths; starts no app).
