"""Plain-language conversation between this component and the Helm hub.

This module deliberately imports nothing from Home Assistant or aiohttp, so it
can be tested on its own with a small fake transport. The Home Assistant side
hands it an aiohttp-backed transport; the tests hand it an in-memory fake that
answers the same routes.

The hub answers these routes under its speaker scope, authenticated with one
bearer token per speaker:

  GET  {base}/speaker/status                  -> {"ok": true, "session": {...}}
  POST {base}/speaker/stt                     wav bytes in,  {"text": ...} out
  POST {base}/speaker/turn                    the spoken words in, an answer or
                                              {"state": "working"} to be polled
  GET  {base}/speaker/turn/<turn_id>?wait=N   the answer once it is ready

A turn is identified by a uuid the component makes up, so a retry of the same
attempt can never run the request twice.
"""

import json
import struct
import time
from urllib.parse import urlencode

try:  # inside the package (Home Assistant)
    from .errors import TransportError
except ImportError:  # run as plain modules from this folder (unit tests)
    from errors import TransportError
# noqa: F401 - re-exported on purpose: one shared error family for every
# transport in this package, and the name stays hubapi.TransportError.

# Answer kinds every caller switches on.
KIND_ANSWER = "answer"  # a sentence to speak
KIND_EMPTY = "empty"  # heard nothing useful
KIND_OFFLINE = "offline"  # the hub could not be reached at all
KIND_ERROR = "error"  # reached the hub, but the call failed
KIND_TIMEOUT = "timeout"  # still working after the whole wait budget
KIND_OK = "ok"  # status call succeeded (config flow)
KIND_BAD_AUTH = "bad_auth"  # the hub refused the token

# Fixed sentences for the cases where no model answer exists. The component
# never makes up words for these: the model is not involved.
OFFLINE_TEXT = "I can't reach Helm right now."
ERROR_TEXT = "That didn't work. Try asking again."
TIMEOUT_TEXT = "That's taking longer than I expected. I'll send the answer to your phone when it's ready."

# Audio: Home Assistant hands us 16 kHz 16-bit signed mono PCM.
SAMPLE_RATE = 16000
MAX_STT_SECONDS = 30  # the hub caps a capture at this; we cut earlier

# Turn timing: the first call long-polls a while, later calls poll shorter.
FIRST_TURN_WAIT = 25
POLL_WAIT = 20
TURN_BUDGET_SECONDS = 90
REQUEST_TIMEOUT = 12  # seconds for the short calls (status, stt)

MAX_BODY_BYTES = SAMPLE_RATE * 2 * (MAX_STT_SECONDS + 5)  # wav sanity guard

# Records and escalation context: bounded payloads, always plain summaries.
MAX_RECORD_BYTES = 32 * 1024   # a record POST is shrunk to fit under this
MAX_CONTEXT_BYTES = 2048       # context carried when escalating a turn
_RECORD_KEEP = 20              # tool calls kept when shrinking a record

# A hub without the /speaker/record route answers 404; remembering that for
# the life of the process keeps every later turn from poking it again.
_RECORD_UNSUPPORTED = False


def _json_bytes(obj):
    return len(json.dumps(obj, ensure_ascii=False).encode("utf-8"))


def _clip_json(obj, limit):
    """Shrink a dict until its JSON form fits under limit bytes: long
    strings first, then the tail of lists. Keys survive where possible."""
    if _json_bytes(obj) <= limit:
        return obj

    def shorten(value, budget):
        if isinstance(value, str):
            return value[:max(40, budget)]
        if isinstance(value, dict):
            return {k: shorten(v, budget // max(1, len(value)))
                    for k, v in value.items()}
        if isinstance(value, list):
            return [shorten(v, budget // max(1, len(value))) for v in value[:8]]
        return value

    clipped = shorten(obj, limit)
    while _json_bytes(clipped) > limit and isinstance(clipped, dict):
        if not clipped:
            break
        clipped.pop(next(reversed(list(clipped))), None)
    return clipped


def _shrink_record(body, limit):
    """Trim a record POST until it fits under limit bytes."""
    if _json_bytes(body) <= limit:
        return body
    for key in ("timings", "tags"):
        if key in body:
            body[key] = {k: v for k, v in list(body[key].items())[:8]} \
                if isinstance(body.get(key), dict) else body.get(key)
    while _json_bytes(body) > limit and body.get("tool_calls"):
        body["tool_calls"] = [
            {"name": c.get("name"), "ok": c.get("ok")}
            for c in body["tool_calls"][:_RECORD_KEEP]]
        break
    if _json_bytes(body) > limit:
        body.pop("timings", None)
        body["tool_calls"] = []
    for key in ("text", "answer"):
        if _json_bytes(body) > limit and isinstance(body.get(key), str):
            body[key] = body[key][:600]
    return body


def pcm_cap(sample_rate=SAMPLE_RATE, seconds=MAX_STT_SECONDS):
    """How many bytes of PCM to keep before cutting a capture off."""
    return sample_rate * 2 * seconds


def wav_from_pcm(pcm, sample_rate=SAMPLE_RATE):
    """Wrap raw 16-bit mono PCM in a WAV header. No third-party library."""
    data_len = len(pcm)
    if data_len > 0xFFFFFFFF:
        raise ValueError("audio too large to wrap as WAV")
    header = (
        b"RIFF"
        + struct.pack("<I", 36 + data_len)
        + b"WAVE"
        + b"fmt "
        + struct.pack("<IHHIIHH", 16, 1, 1, sample_rate, sample_rate * 2, 2, 16)
        + b"data"
        + struct.pack("<I", data_len)
    )
    return header + pcm


def ends_with_question(text):
    """HA keeps the microphone open when this is true for a reply.

    HA itself has no automatic rule; the agent decides. We ask again only when
    the reply plainly ends in a question mark, so the speaker does not sit and
    listen after an ordinary answer.
    """
    return text.rstrip().endswith("?")


def session_elevated(session, now=None):
    """Is the hub session currently raised by a passcode?

    Accepts whatever shape the hub sends: an ``elevated`` flag, or an
    ``elevatedUntil`` (or ``until``) epoch-seconds deadline. Absent or odd
    means not elevated.
    """
    if not isinstance(session, dict):
        return False
    if session.get("elevated") is True:
        return True
    until = session.get("elevatedUntil", session.get("until"))
    if isinstance(until, (int, float)) and not isinstance(until, bool):
        return (now if now is not None else time.time()) < until
    return False


def build_presence(area=None, people=None, occupied=None):
    """A small snapshot of the room for the hub's registry (it only records
    this in the first version; nothing routes on it yet). Plain data in,
    plain data out, so it is testable without Home Assistant."""
    presence = {}
    if area:
        presence["area"] = str(area)
    if people is not None:
        presence["people"] = [str(name) for name in people]
    if occupied is not None:
        presence["occupied"] = bool(occupied)
    return presence


class HubApi:
    """The four speaker routes, plus the turn polling state machine."""

    def __init__(self, base_url, token, transport, clock=time.monotonic):
        self.base = (base_url or "").rstrip("/")
        self.token = token
        self.transport = transport
        self.clock = clock

    def _auth(self):
        return {"Authorization": "Bearer " + (self.token or "")}

    def _params(self, device=None, room=None, hint=None, wait=None):
        params = {}
        if device:
            params["device"] = str(device)
        if room:
            params["room"] = str(room)
        if hint:
            params["hint"] = str(hint)
        if wait is not None:
            params["wait"] = str(int(wait))
        return params

    def _url(self, path, params=None):
        url = self.base + path
        if params:
            url += "?" + urlencode(params)
        return url

    @staticmethod
    def _json(body):
        try:
            data = json.loads(body.decode("utf-8"))
        except (ValueError, AttributeError, UnicodeDecodeError):
            return None
        return data if isinstance(data, dict) else None

    async def status(self):
        """Reachability and token check for the config flow, plus the hub's
        current session (used for the LED)."""
        try:
            status, body = await self.transport.request(
                "GET", self._url("/speaker/status"),
                headers=self._auth(), timeout=REQUEST_TIMEOUT)
        except TransportError:
            return {"kind": KIND_OFFLINE}
        if status in (401, 403):
            return {"kind": KIND_BAD_AUTH}
        if status != 200:
            return {"kind": KIND_ERROR}
        data = self._json(body)
        if data is None or data.get("ok") is not True:
            return {"kind": KIND_ERROR}
        return {"kind": KIND_OK, "session": data.get("session")}

    async def transcribe(self, wav, device=None, room=None, hint=None):
        """Send one wav capture; returns
        ``{"kind": "answer", "text": ...}``, ``{"kind": "empty"}`` or an
        error/offline kind. A passcode attempt comes back as the hub's
        placeholder text — that is by design: the digits never leave the hub,
        and the agent call carrying the placeholder is answered there too."""
        headers = self._auth()
        headers["Content-Type"] = "audio/wav"
        try:
            status, body = await self.transport.request(
                "POST", self._url("/speaker/stt", self._params(device, room, hint)),
                headers=headers, data=wav, timeout=REQUEST_TIMEOUT)
        except TransportError:
            return {"kind": KIND_OFFLINE}
        if status in (401, 403):
            return {"kind": KIND_BAD_AUTH}
        if status != 200:
            return {"kind": KIND_ERROR}
        data = self._json(body)
        if data is None:
            return {"kind": KIND_ERROR}
        state = data.get("state")
        if state == "empty":
            return {"kind": KIND_EMPTY}
        if state in ("error", "offline"):
            return {"kind": KIND_OFFLINE if state == "offline" else KIND_ERROR}
        text = data.get("text")
        if isinstance(text, str) and text.strip():
            return {"kind": KIND_ANSWER, "text": text}
        return {"kind": KIND_EMPTY}

    async def run_turn(self, text, turn_id, device=None, room=None, area=None,
                       hint=None, presence=None, budget=TURN_BUDGET_SECONDS,
                       first_wait=FIRST_TURN_WAIT, poll_wait=POLL_WAIT,
                       context=None, escalation=False):
        """Ask the hub one turn and return
        ``{"kind": "answer", "text": ..., "session": ...}``, or one of the
        fixed-sentence kinds (``offline``, ``error``, ``timeout``). Unknown
        hub states count as errors: fail closed. An error reached over http
        also carries ``"status"`` (the reply code), so a caller can tell a
        busy hub from a refused token; an unreachable hub or a broken body
        has no code to give."""
        body = {
            "turn_id": turn_id,
            "text": text,
            "wait": int(first_wait),
        }
        if device:
            body["device"] = str(device)
        if room:
            body["room"] = str(room)
        if area:
            body["area"] = str(area)
        if hint:
            body["hint"] = str(hint)
        if presence:
            body["presence"] = presence
        if context is not None:
            # what the little brain knew locally (room facts, last few
            # exchanges); clipped so an escalation never carries a blob
            body["context"] = _clip_json(context, MAX_CONTEXT_BYTES)
        if escalation:
            body["origin"] = "escalation"
        headers = self._auth()
        headers["Content-Type"] = "application/json"
        try:
            started = self.clock()
            status, raw = await self.transport.request(
                "POST", self._url("/speaker/turn"),
                headers=headers, data=json.dumps(body).encode("utf-8"),
                timeout=first_wait + 5)
        except TransportError:
            return {"kind": KIND_OFFLINE}
        if status in (401, 403):
            # the http status rides along so the caller can tell a setup
            # mistake from plain bad luck (and say "busy" for a 429)
            return {"kind": KIND_ERROR, "status": status}
        if status == 202:
            return await self._poll(turn_id, started, budget, poll_wait)
        if status != 200:
            return {"kind": KIND_ERROR, "status": status}
        return self._answer(raw)

    async def _poll(self, turn_id, started, budget, poll_wait):
        url = self._url("/speaker/turn/" + turn_id)
        while True:
            remaining = budget - (self.clock() - started)
            if remaining <= 0:
                return {"kind": KIND_TIMEOUT}
            wait = min(int(poll_wait), int(remaining))
            if wait <= 0:
                return {"kind": KIND_TIMEOUT}
            try:
                status, raw = await self.transport.request(
                    "GET", url + "?" + urlencode({"wait": str(wait)}),
                    headers=self._auth(), timeout=wait + 5)
            except TransportError:
                return {"kind": KIND_OFFLINE}
            if status in (401, 403):
                return {"kind": KIND_ERROR, "status": status}
            if status == 202:
                continue
            if status == 404:
                # The hub restarted or dropped the turn; do not resend.
                return {"kind": KIND_ERROR, "status": status}
            if status != 200:
                return {"kind": KIND_ERROR, "status": status}
            return self._answer(raw)

    def _answer(self, raw):
        data = self._json(raw)
        if data is None:
            return {"kind": KIND_ERROR}
        state = data.get("state", "answer")
        if state == "working":
            return {"kind": KIND_ERROR}
        if state == "offline":
            return {"kind": KIND_OFFLINE}
        if state != "answer":
            return {"kind": KIND_ERROR}
        text = data.get("text")
        if not isinstance(text, str) or not text.strip():
            return {"kind": KIND_ERROR}
        return {"kind": KIND_ANSWER, "text": text.strip(),
                "session": data.get("session")}

    async def record(self, *, turn_id, device=None, room=None, area=None,
                     text=None, answer=None, kind=None, tool_calls=None,
                     timings=None, model=None, tags=None):
        """Best-effort turn record for the hub's benchmark store (R1). A
        turn id makes retries idempotent. This route may not exist: a 404
        switches us off for the process, and nothing here ever raises -
        speech already happened; the record is decoration."""
        global _RECORD_UNSUPPORTED  # noqa: PLW0603 - one switch per process
        if _RECORD_UNSUPPORTED or not turn_id:
            return {"kind": "unsupported"}
        body = {"turn_id": turn_id, "ts": round(time.time(), 3)}
        for key, value in (("device", device), ("room", room), ("area", area),
                           ("text", text), ("answer", answer), ("kind", kind),
                           ("model", model)):
            if value is not None:
                body[key] = value
        if tool_calls:
            body["tool_calls"] = list(tool_calls)[:_RECORD_KEEP * 2]
        if timings:
            body["timings"] = timings
        if tags:
            body["tags"] = tags
        _shrink_record(body, MAX_RECORD_BYTES)
        headers = self._auth()
        headers["Content-Type"] = "application/json"
        try:
            status, _raw = await self.transport.request(
                "POST", self._url("/speaker/record"),
                headers=headers, data=json.dumps(body).encode("utf-8"),
                timeout=REQUEST_TIMEOUT)
        except TransportError:
            return {"kind": KIND_OFFLINE}
        if status == 404:
            _RECORD_UNSUPPORTED = True
            return {"kind": "unsupported"}
        if status in (401, 403):
            return {"kind": KIND_BAD_AUTH}
        if status != 200:
            return {"kind": KIND_ERROR}
        return {"kind": KIND_OK}
