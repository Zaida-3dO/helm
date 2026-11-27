"""Per-stage timing records for the benchmark (plan section 5).

One record per turn, written through timings.TimingWriter. The fields are
fixed: ``stage_ms`` holds durations, ``t`` absolute millisecond stamps.
A field stays null when no component can observe it - we never invent a
number. What this build can really see:

* the speaker capture and transcription window (stt.py hands its own
  measurements over through runtime_data["stt_last"]);
* prompt, model and tool durations from agent_core's TurnResult;
* wake word, speaker state changes and TTS are not observable from inside
  this integration (the assist pipeline pushes those only to websocket
  subscribers), so they arrive as null unless a caller supplies them.

The pure part (StageLog) runs without Home Assistant; start() wires it to
a live hass.
"""

import logging
import time

from .timings import TimingWriter

_LOGGER = logging.getLogger(__name__)

TIMINGS_DIRNAME = "helm_speaker"
STT_FRESH_SECONDS = 15.0  # join a capture only if it just happened


class StageLog:
    """Assembles and writes one record per turn."""

    def __init__(self, writer):
        self.writer = writer

    def finish_turn(self, *, turn_id, device=None, profile=None, result,
                    kind=None, tags=None, text_in=None, text_out=None,
                    stt=None, sat=None, now=None):
        """result: agent_core TurnResult (or anything exposing the same
        attributes). stt/sat are optional dicts from whoever can observe
        those stages. Keys not seen are null."""
        now = time.time() if now is None else now
        timings = getattr(result, "timings", None) or {}
        model_ms = timings.get("model_ms") or []
        tool_ms = timings.get("tool_ms") or []
        stt = stt or {}
        sat = sat or {}

        t_wake = _ms(stt.get("listen_start_ts"))
        t_stt_done = _ms(stt.get("done_ts"))
        t_model_start = None
        if timings.get("total_ms") is not None and model_ms:
            # the agent knows when its first call began, relative to turn
            # start; only an upper bound against "now" is available here
            t_model_start = int(now * 1000) - int(timings["total_ms"]) \
                + (int(timings.get("prompt_ms") or 0))
        t_answer_ready = int(now * 1000)

        stage_ms = {
            "wake_to_listen": None,
            "speech": _sec_ms(stt.get("listened_s")),
            "stt": _sec_ms(stt.get("stt_ms")),
            "prompt": timings.get("prompt_ms"),
            "model": int(sum(model_ms)) if model_ms else None,
            "tool": [int(v) for v in tool_ms] or None,
            "answer_ready": timings.get("total_ms"),
            "tts": None,
        }
        t = {
            "wake": t_wake,
            "speech_end": _ms(stt.get("collected_ts")),
            "stt_done": t_stt_done,
            "model_start": t_model_start,
            "model_done": None,  # not separately observed; model_ms gives the sum
            "tool_done": None,
            "answer_ready": t_answer_ready,
            "tts_start": _ms(sat.get("tts_start_ts")),
            "tts_end": _ms(sat.get("tts_end_ts")),
        }

        record = {
            "ts": round(now, 3),
            "turn_id": turn_id,
            "device": device or None,
            "profile": profile,
            "model": getattr(result, "model_used", None),
            "prompt_hash": getattr(result, "prompt_hash", None),
            "tags": tags or {},
            "stage_ms": stage_ms,
            "t": t,
            "tool_calls": getattr(result, "tool_calls", None) or [],
            "kind": kind or getattr(result, "kind", None),
        }
        if text_in is not None:
            record["text_in"] = text_in
        if text_out is not None:
            record["text_out"] = text_out
        # A5 in review terms: the model said nothing but a tool did the work,
        # and we spoke the tool's own line - worth marking on the row so the
        # benchmark can tell that answer apart from a generated one
        if getattr(result, "empty_final", False):
            record["empty_final"] = True
        # the home-control gate: words forced back through a tool call
        # (guard_reprompt) and/or a success claim replaced by our own honest
        # sentence (guard_blocked) - rows the benchmark must not count as clean
        if getattr(result, "guard_reprompt", False):
            record["guard_reprompt"] = True
        if getattr(result, "guard_blocked", False):
            record["guard_blocked"] = True
        self.writer.append(record)
        return record


def _ms(ts_seconds):
    return None if ts_seconds is None else int(float(ts_seconds) * 1000)


def _sec_ms(seconds):
    return None if seconds is None else int(round(float(seconds) * 1000))


def start(hass, entry):
    """Create the stage log for this entry (called from async_setup_entry)."""
    path = hass.config.path(TIMINGS_DIRNAME, "timings.jsonl")
    log = StageLog(TimingWriter(path))
    entry.runtime_data["stages"] = log
    _LOGGER.debug("helm_speaker timings go to %s", path)


def take_stt(entry, done_not_before):
    """Pop the stt.py measurement if it is fresh enough to belong to this
    turn (single-speaker join: the newest capture wins)."""
    last = (entry.runtime_data or {}).pop("stt_last", None)
    if not last:
        return None
    try:
        if float(last.get("done_ts") or 0.0) < done_not_before:
            return None
    except (TypeError, ValueError):
        return None
    return last


def note_turn(hass, entry, *, device_id, turn_id, text_in, text_out, kind,
              result):
    """Best effort: a timing record must never break or delay speech."""
    log = (entry.runtime_data or {}).get("stages")
    if log is None:
        return
    options = entry.options or {}
    profile = options.get("profile") or "path_a"
    stt = take_stt(entry, time.time() - STT_FRESH_SECONDS)

    def write():
        try:
            log.finish_turn(
                turn_id=turn_id, device=device_id, profile=profile,
                result=result, kind=kind, tags=getattr(result, "tags", None),
                text_in=text_in, text_out=text_out, stt=stt)
        except Exception:  # noqa: BLE001 - bookkeeping after speech
            _LOGGER.exception("could not write timing record for %s",
                              turn_id[:8])

    if hass is not None and hasattr(hass, "async_add_executor_job"):
        # the append is disk I/O: keep it off the event loop (Home Assistant
        # warns loudly about blocking writes there). Fire and forget - write()
        # swallows its own failures.
        hass.async_add_executor_job(write)
    else:
        write()  # plain-python tests and replay: no loop to protect
