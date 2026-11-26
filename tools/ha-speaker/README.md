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
| `homecontrol.py` | the four house tools and what they may change |
| `ha_backend.py` | the Home Assistant side of those tools (expose check, timers) |
| `conversation.py` | the conversation agent entity that runs one spoken turn |
| `stt.py` | speech-to-text stage that wraps audio for the hub (hub entries only) |
| `hubapi.py` | all hub talking in one plain-Python module |
| `profiles.py` | entry data + options in, one model config out; keeps keys quiet |
| `timings.py`, `stages.py`, `replay.py` | the benchmark: timing lines, record joiner, replay door |
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
  each area (capped per area with a "(+N more)" tail), then up to three rounds
  of house tools.
- The house tools are `light`, `climate`, `timer` and `home_state`. Timers go
  through Home Assistant's built-in voice-timer intents on the asking speaker,
  so they live on the satellite rather than as helper entities. Names match by
  area first, then exact name, then a forgiving spelling; an ambiguous name is
  asked back, never guessed.
- A small model trained on Home Assistant's own service data sometimes writes
  those argument names instead of ours — `color_temp_kelvin` and `hvac_mode`
  are accepted as aliases of `color_temp` and `mode`. Everything for one
  "turn it on at sixty percent, warm" lands in a single `light.turn_on`; two
  calls would flicker. Zero brightness means off.
- Entities you have hidden, disabled, or filed under a config/diagnostic
  category are invisible here even if an old expose switch happens to be on.
- One spoken ask may touch at most twelve devices and make at most four write
  calls; a second sentence starts a fresh budget. A request that would
  rewrite the whole house in one breath is refused politely instead.
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

## Benchmark hooks

Every turn appends one JSON line to `<config>/helm_speaker/timings.jsonl`
(stage durations, prompt hash, tool calls). `input_text.helm_speaker_tags` may
carry tags like `arm=A run=3`, which ride into every row. The
`helm_speaker.replay` service feeds a JSONL set of sentences through the real
pipeline — the file must be one Home Assistant is allowed to read (its config
folder by default), it runs dry unless you pass `act: true`, and each row gets
a fresh change budget. Outside Home Assistant, `python replay.py --set ...`
runs the same engine against a JSON house description, always dry.

## Tests

```
cd tools/ha-speaker
PYTHONDONTWRITEBYTECODE=1 python -B -m unittest \
    test_hubapi test_agent_core test_homecontrol test_ha_glue test_stages test_record
```

Standard library only — no installs, no network. They cover the wire shapes,
the turn engine (including a model that misparses tools or says nothing), the
house rules above, profile switching and secrecy, and the timing records. The
screens and entities themselves need a running Home Assistant; those run as a
separate scratch-install check, not in this suite.
