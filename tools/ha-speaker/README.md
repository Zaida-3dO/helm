# Helm Speaker — a Home Assistant integration

This folder is a Home Assistant **custom integration** (written to be copied
into `custom_components/`, not published to PyPI). It lets a Home Assistant
voice pipeline — the kind that runs on a small smart speaker — ask **Helm**
instead of Home Assistant's own assistant for the harder questions.

Home Assistant stays in the middle and keeps doing what it is good at:

- it hears the wake word and the audio,
- its own local commands (lights, timers) still answer first, without Helm,
- it speaks the reply with whatever local text-to-speech you have set up.

Helm does the thinking: it transcribes the audio with its own speech model,
holds the conversation, works out what the speaker is allowed to do, and
answers. Nothing in Home Assistant ever sees a spoken passcode: the hub
replaces such a capture with a placeholder and answers it itself.

## Files

| File | What it is |
|---|---|
| `manifest.json`, `strings.json` | integration metadata and setup-screen wording |
| `config_flow.py` | the setup screen (address, token, a few labels) |
| `stt.py` | speech-to-text stage: wraps the audio and asks the hub |
| `conversation.py` | the assistant: sends the words, returns the reply to be spoken |
| `hubapi.py` | all hub talking in one plain-Python module (this is what the tests cover) |
| `__init__.py` | wiring: sessions, device entry, the unlock light |
| `test_hubapi.py` | tests, standard library only |

## Installing

1. Copy this folder to `<config>/custom_components/helm_speaker/` on your
   Home Assistant machine (the folder name must be `helm_speaker`).
2. Restart Home Assistant.
3. Settings → Devices & Services → Add Integration → **Helm Speaker**.

## The setup screen

| Field | Meaning |
|---|---|
| Hub address | where the Helm hub listens, e.g. `http://hub.example:8480` |
| Speaker token | the token Helm gives this speaker; it authorises exactly this speaker, nothing else |
| Room label (optional) | a name for the room, recorded by the hub |
| Identity hint (optional) | kept for a future second wake word; not used yet |
| Unlock light (optional) | a light entity that glows cyan while the session is unlocked by a passcode, and is restored afterwards |

An `http://` address is refused unless the "Allow plain http" box is ticked, because the
token would cross the network readable; prefer `https://`.

Setup calls the hub's status route before saving, so a wrong address or
token is refused at once. Only one entry per hub is allowed.

## Wiring it into a voice pipeline

Settings → Voice assistants → add an **Assist pipeline**:

- **Speech-to-text**: the `Helm speaker` entity (`stt.helm_speaker`)
- **Conversation agent**: the `Helm speaker` entity (`conversation.helm_speaker`)
- **Text-to-speech**: any local voice you already have

Assign the pipeline to your speaker. Keeping a separate pipeline with Home
Assistant's own agent as a fallback is a good idea: if Helm is off, the
speaker can still use it.

## How it behaves

- Each stage logs how long it took at info level (`helm_speaker timing: ...`), so a slow answer can be pinned on the
  speech call, the agent call (the model's time) or Home Assistant's own text-to-speech. Turn on info logs for
  `custom_components.helm_speaker` to see them.

- Each question gets a fresh random id, so a retry can never run the same
  request twice on the hub.
- The first answer attempt waits about 25 seconds; while the hub says it is
  still working the component keeps asking, up to about 90 seconds in total.
  After that the speaker says so — the hub sends the finished answer to the
  person's phone instead.
- When the hub cannot be reached the speaker says, in one fixed sentence,
  that it cannot reach Helm. Errors get their own fixed sentence. These
  lines are written by this component, never by a model.
- A reply that ends in a question mark reopens the microphone, so you can
  answer back; every other reply ends the exchange.
- With no unlock light configured there is no indicator; everything else
  works the same.

## Limits of this first version

- Replies arrive whole; they are not streamed word by word yet.
- The hub cannot yet make the speaker say things on its own.
- Home Assistant does not tell the speech-to-text stage which speaker the
  audio came from, so captures are keyed by the token and the room label;
  the question right after them carries the real device.

## Tests

```
cd tools/ha-speaker
python -m unittest test_hubapi
```

Standard library only — no installs, no network. The tests cover the wire:
audio wrapping, every hub reply shape, the polling state machine, the fixed
sentences and the unlock check. The Home Assistant screens and entities
themselves need a running Home Assistant and are checked by hand.
