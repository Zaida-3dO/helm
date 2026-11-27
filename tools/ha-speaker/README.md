# Helm Speaker — a Home Assistant integration

This folder is a Home Assistant **custom integration** (written to be copied
into `custom_components/`, not published to PyPI). It lets a Home Assistant
voice pipeline — the kind that runs on a small smart speaker — talk to a small
chat model directly, and to a Helm hub for the personal or long questions.

Home Assistant stays in the middle and keeps doing what it is good at: it
hears the wake word and the audio, its own local commands still answer first,
and it speaks the reply with whatever local text-to-speech you set up.

## Files

| File | What it is |
|---|---|
| `manifest.json`, `strings.json`, `translations/en.json` | integration metadata and screen wording |
| `config_flow.py` | setup screen and the options screens (profiles, dry run) |
| `agent_core.py` | the turn engine: prompt, tool rounds, retries, timings |
| `homecontrol.py` | the five house tools and what they may change |
| `ha_backend.py` | the Home Assistant side of those tools (expose check, timers) |
| `conversation.py` | the conversation agent entity that runs one spoken turn |
| `stt.py` | speech-to-text stage that wraps audio for the hub (hub entries only) |
| `hubapi.py` | all hub talking in one plain-Python module |
| `profiles.py` | entry data + options in, one model config out; keeps keys quiet |
| `timings.py`, `stages.py`, `replay.py` | the benchmark: timing lines, record joiner, replay door |
| `error_lines.json`, `errorlines.py` | the pre-recorded failure lines: one catalogue plus the decision table that picks one |
| `render_error_lines.py` | renders that catalogue's flac files into Home Assistant's `www` folder |
| `errors.py` | the one transport error family every caller shares |

## Installing

1. Copy this folder to `<config>/custom_components/helm_speaker/` (the folder
   name must be `helm_speaker`).
2. Restart Home Assistant.
3. Settings → Devices & Services → Add Integration → **Helm Speaker**.

## The setup screen

| Field | Meaning |
|---|---|
| Model address | an OpenAI-style chat-completions endpoint, e.g. `http://gpu.example:11434/v1/chat/completions` — this is the brain and it is required |
| Model name / key | what to call in the request, and a bearer key if the server wants one |
| Model thinking on | when ticked, the request body carries `"think": true`; some servers spell it differently — put your own spelling in the extra-body box and it wins |
| Extra headers / body (JSON) | e.g. a priority-lane header, or a server-specific thinking switch |
| Hub address / token | optional; without them the speaker still does home control, it just cannot take personal questions |
| Room label, identity hint, unlock light | optional labels and the passcode indicator |
| Chores feed address / token | optional pair for the spoken "what's due" check-in; the token should be a read-only one — it can fetch the lists and nothing else. Unset: the speaker just says so plainly |

Plain `http://` addresses are refused unless "Allow plain http" is ticked —
what you say (and the speaker token) would otherwise travel unencrypted on
your network. A hub that answers with a bad token is refused at setup; an
unreachable one is accepted, because escalation degrades to one sentence while
everything else keeps working. Only one entry per install is allowed.

## Profiles and dry run (the options screens)

Settings → the integration → Configure opens two steps: first pick which
profile (`path_a` or `path_b`) to edit, whether to make one active, and a
**dry run** switch; then the model fields for the profile you picked appear,
prefilled from that profile — what you see is what gets saved. A blank key
field keeps the stored key; keys are never shown back and never logged beyond
the model name and host. `helm_speaker.set_profile` switches profiles without
a restart too. Dry run makes every house tool say what it would do and change
nothing — useful while testing what a small model actually asks for.

## How a turn behaves, in plain words

- Home Assistant's own local intents answer first when they match; anything
  else reaches this agent: rules, the asking room and the exposed names of
  each area (capped per area with a "(+N more)" tail), then up to four model
  rounds over the house tools.
- A house request does not become words until a tool call has succeeded
  behind it - and for an ask that changes something, only a successful
  WRITE counts as standing behind it (a read that happened to work proves
  nothing about the change). If the model answers one with no tool call at
  all, it is sent back once with the tool forced; if still nothing runs, a
  fixed "I couldn't do that." goes instead of any success claim. When every
  call failed, the spoken line is built from the tools' own reasons plus the
  options they offered, never from the model's wording; when only part of
  the ask worked, the failure reason is appended to the model's own sentence.
  A single clean question ("Which room?") or an honest "sorry, I didn't
  catch that" claims no success and is spoken as-is - forcing a guessed
  action over those is exactly what this guard exists to prevent. And the
  gate only engages sentences that really ask something of the house: a
  joke about a lamp, a reminder for mum or the hour the shop opens sail
  through untouched, and a tool is only forced when the sentence names its
  target. Those rows carry `guard_reprompt` / `guard_blocked` in the timing
  record; an endpoint that refuses the forced round or answers it with
  nothing gets the fixed sentence, not a second unguarded brain.
- The house tools are `light`, `climate`, `timer`, `home_state` and
  `home_status`. Timers go through Home Assistant's built-in voice-timer
  intents on the asking speaker, so they live on the satellite rather than as
  helper entities. Names match areas first: a PLURAL room word ("the office
  lights", "all the kitchen lights") means every exposed light of that room
  in ONE call - even when one bulb is itself named after the room. A
  singular lamp/light/bulb keeps the exact entity name, so a real "office
  lamp" is found by name instead of sweeping up a room called Office. Then
  comes forgiving spelling; an ambiguous name is asked back, never guessed.
  Colour-temperature asks are not limited to bulbs advertising `color_temp`:
  Home Assistant emulates it on the other colour bulbs too, so only
  fixed-white, brightness-only and on/off bulbs are left out with a plain
  per-light note while the others move: a partial result saying what worked
  and which ones could not.
- `home_status` answers "what chores are due / what needs water" from the
  optional chores feed: at most three names, overdue first, one or two short
  sentences. It only ever reads — nothing is marked done by voice — and a
  read never spends the ask's change budget. Feed down or unset speaks one
  honest sentence instead of an error.
- A small model trained on Home Assistant's own service data sometimes writes
  those argument names instead of ours — `color_temp_kelvin` and `hvac_mode`
  are accepted as aliases of `color_temp` and `mode`. Everything for one
  "turn it on at sixty percent, warm" lands in a single `light.turn_on`; two
  calls would flicker. Zero brightness means off.
- Entities you have hidden, disabled, or filed under a config/diagnostic
  category are invisible here even if an old expose switch happens to be on.
- One spoken ask may touch at most twelve devices and make at most six write
  calls (a whole room counts as one); a second sentence starts a fresh
  budget. A request that would rewrite the whole house in one breath is
  refused politely instead.
- If no room is named, the asking room (the satellite's area) is meant; the
  prompt says so and the house clock arrives in the local zone.
- A personal question ("what is on my calendar") calls `ask_helm` — only when
  a hub is configured; with no hub the tool and its rule are simply absent,
  and the model answers or declines by itself. The hub round gets about
  eighteen seconds; if it does not answer, one fixed sentence goes instead of
  an open-ended wait.
- When the tools did work but the model then said nothing (thinking ate the
  answer), the tool's own sentence is spoken — never a denial of something
  that happened.

## Pre-recorded error lines

Every way a spoken turn can fail or end without an answer has a short
pre-recorded line in `error_lines.json`: code, words, flac file, situations,
and which fixed sentence of this package it stands in for (`agent_core` and
`hubapi` constants; a test fails when a new fixed sentence appears without a
line). The texts stay under twelve words, calm, no jargon.

Who says what: the component DECIDES and STORES. When a turn ends on one of
these codes, the asking satellite resolves to an `assist_satellite` entity
and the deployment's `input_text.helm_speaker_line` helper exists, the
response carries an empty string — the voice pipeline then skips synthesis
and ends cleanly — and the code is stored in that helper by a blocking
service call before the turn returns. (Ordering matters: an event-triggered
automation run is scheduled one loop tick *after* the state trigger that
ends this turn, so letting an event store it would let the automation read
an empty helper.) A `helm_speaker_line` event goes out too, for debugging
only. The companion Home Assistant automation, watching processing hand to
idle, takes the stored code and plays `<config>/www/<file>` with
`assist_satellite.announce`. The component never announces from inside its
own turn: an announcement cancels the running pipeline, and a delayed one
races the automation into double speech. Automation-side lines
(`did_not_catch_that`, `speech_down`, `stuck_try`) are decided there, not
here — one probe each for hearing (:18950) and voice (:18960) tells
"hearing down", "voice down" and "machine down" apart; a turn that ended
with no stored code and both probes healthy stays quiet (that is what a
spoken "never mind" or "cancel" should do); and a satellite stuck in
processing for a full minute gets its own line whose announcement cancels
the hung run.

Fallback: no code (a live answer, a text or REST turn with no satellite...),
no `assist_satellite` entity on the device, no helper entity in this install
or a store call that fails, or a missing/unreadable json — the turn speaks
its fixed sentence live by text-to-speech exactly as before this feature.
History rows, benchmark stages and hub records keep the canonical sentence
either way; only the pipeline's input goes blank when a file replaces it.

The one exact-match line (`helm_needs_updating`) compares the hub's answer to
its "too old to answer by voice" sentence word for word — fragile by nature:
if the hub ever rewords it, that turn just speaks live words again. The log
says so when http 401/403 refuses a turn (a wrong speaker token or scope is a
setup mistake).

Situations deliberately left with no line: wake word plus silence stays
silent; tool refusals the model words around variable device names ("which
one did you mean?") are normal answers, not failures; a truncated long answer
and "the tool worked and the model said Done." are normal too.

Render after each voice change (run on the machine whose voice service
answers, where Home Assistant's `www` folder is reachable):

```
python render_error_lines.py --www <config>/www          # only what is missing
python render_error_lines.py --www <config>/www --force  # after editing default_voice.txt
python render_error_lines.py --www <config>/www --check  # exits non-zero if any file is missing
```

The speech service must be on the same machine (loopback only); ffmpeg does
the wav→flac conversion. `helm_speaker_offline_alba.flac` predates this
catalogue and is reused by the hub-unreachable line, so a plain render never
touches it.

## Benchmark hooks

Every turn appends one JSON line to `<config>/helm_speaker/timings.jsonl`
(stage durations, prompt hash, tool calls). `input_text.helm_speaker_tags` may
carry tags like `arm=A run=3`, which ride into every row. The
`helm_speaker.replay` service feeds a JSONL set of sentences through the real
pipeline — the file must be one Home Assistant is allowed to read (its config
folder by default), it runs dry unless you pass `act: true`, and each row gets
a fresh change budget. Outside Home Assistant, `python replay.py --set ...`
runs the same engine against a JSON house description, always dry; a model
key goes in via `--api-key-env`, never on the command line.

## Tests

```
cd tools/ha-speaker
PYTHONDONTWRITEBYTECODE=1 python -B -m unittest \
    test_hubapi test_agent_core test_homecontrol test_ha_glue test_stages test_record test_error_lines
```

Standard library only — no installs, no network. They cover the wire shapes,
the turn engine (including a model that misparses tools or says nothing), the
house rules above, profile switching and secrecy, the timing records, and the
error-line decision table plus the render command against a fake speech
service and a stand-in ffmpeg. The screens and entities themselves need a
running Home Assistant; those run as a separate scratch-install check, not in
this suite.
