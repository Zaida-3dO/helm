"""The speaker's conversation brain: a lean prompt, a model client, a tool loop.

This module deliberately imports only the standard library - nothing from
Home Assistant, no aiohttp, not even ``hubapi`` (a couple of small helpers are
copied on purpose so the two modules stay independently testable). The caller
injects a transport with the same shape as ``hubapi``'s::

    await transport.request(method, url, headers=..., data=..., timeout=...)
        -> (status:int, body:bytes)
    raises TransportError when the request never got an answer

so production hands it an aiohttp-backed transport and tests hand it an
in-memory fake. The replay tool runs exactly this code against a real model
endpoint - there is no separate test brain.

The model talks OpenAI-style chat completions with function tools; the URL,
the model name, extra headers (that is how a priority lane is selected) and
extra body keys (that is how thinking switches travel) are config values,
never hardcoded here.

One turn in, one TurnResult out. Every failure that ends up spoken is one of
our own fixed sentences; the model never words those.
"""

import hashlib
import json
import re
import time
import unicodedata
from dataclasses import dataclass, field


try:  # inside the package (Home Assistant) or a test package
    from .errors import RequestTimeout, TransportError
except ImportError:  # run as plain modules from this folder (unit tests)
    from errors import RequestTimeout, TransportError
# noqa: F401 - re-exported:
# every transport in this package raises these exact classes (see errors.py);
# the name stays reachable as agent_core.TransportError for callers.


# Answer kinds a caller switches on. "escalate" means: not ours to answer -
# send it to the hub, carrying escalate_text.
KIND_ANSWER = "answer"
KIND_ESCALATE = "escalate"
KIND_OFFLINE = "offline"
KIND_ERROR = "error"
KIND_TIMEOUT = "timeout"

# Fixed sentences. Written by us, never generated: a spoken failure must be
# short, calm and the same every time.
OFFLINE_TEXT = "My thinking side is offline right now."
ERROR_TEXT = "That didn't work. Try asking again."
TIMEOUT_TEXT = "That's taking longer than I expected. Try asking again."
EMPTY_TEXT = "I didn't catch that."
CONTROL_TEXT = "I couldn't do that."

# --------------------------------------------------------------------------
# the home-control gate: some sentences MUST be carried out by a tool before
# any words are spoken, because a small model happily says "done" without
# ever calling one. The classifier is a word table over the request text -
# deliberately no model call; a wrong "no" here only lets an honest answer
# through, while the gate itself never trusts a tool-less claim of success.

_CONTROL_OBJECTS = frozenset("""
light lights lighting lamp lamps bulb bulbs heating heater radiator boiler
thermostat timer timers blind blinds curtain curtains shutter shutters
""".split())

# No bare "on"/"off": they would drag personal questions ("what is on my
# calendar") into the house tools. A spoken command names an object or a real
# control verb anyway; a miss here only falls back to today's behaviour,
# while the gate never trusts tool-less words when it does fire.
_CONTROL_ACTIONS = frozenset("""
turn turns turned toggle switch switches dim dims brighten brightens darken
brighter dimmer brightness colour color hue kelvin temperature set sets
open closes
""".split())

# Verbs that ask for something to be changed (as opposed to colour/kelvin and
# the rest, which are nouns a read question can carry too). A write request is
# only carried by a tool call that actually wrote.
_WRITE_VERBS = frozenset("""
turn turns turned toggle switch switches dim dims brighten brightens darken
set sets put puts make makes open closes
""".split())

_WORD_RE = re.compile(r"[a-z0-9']+")


def is_home_control(text):
    """True when the request words name a house object or a control action.
    Reads count too - 'what temperature is the office' should come from the
    house, not from the model's memory of last turn."""
    if not isinstance(text, str) or not text.strip():
        return False
    tokens = set(_WORD_RE.findall(text.lower()))
    return bool(tokens & (_CONTROL_OBJECTS | _CONTROL_ACTIONS))


# Each recorded tool call carries its character so the gate can tell "the ask
# really happened" from "a read happened to work". Everything that can change
# the house is a write; status actions and the two room reads are reads.
_WRITE_TOOLS = ("light", "climate", "timer")


def _call_kind(name, args):
    if name in ("home_state", "home_status"):
        return "read"
    if name in _WRITE_TOOLS:
        action = str((args or {}).get("action") or "").lower()
        return "read" if action == "status" else "write"
    return "write"  # an unknown tool attempted a change until proven otherwise


# Words that read like the ask was carried out. A reply carrying one may not be
# spoken over a turn nothing wrote; a plain ask-back never carries one.
_CLAIM_WORDS = frozenset("""
done already finished all set turned switched changed made put moved
opened closed off on now ready
""".split())


def _is_claim(text):
    return bool(set(_WORD_RE.findall(str(text).lower())) & _CLAIM_WORDS)


def _pure_question(reply):
    """One clean question and nothing else: ends in '?', no other sentence end
    inside, and no done-words. "Which room?" yes; "Done, the lights are red
    now. Anything else?" no - that hides a success claim behind a question."""
    if not ends_with_question(reply):
        return False
    if re.search(r"[.!]", reply):
        return False
    return not _is_claim(reply)


# Honest non-doing words ("Sorry, I didn't catch what to turn off."). They
# promise no success; forcing a guessed action over one of them would act on
# a mishearing, which is exactly what must never happen.
_DECLINE_WORDS = frozenset("""
sorry cannot cant can't dont don't didnt didn't unsure unclear misheard
missed catch nope
""".split())

_DECLINE_PHRASE = re.compile(r"\b(not sure|no idea|i missed)\b")


def _declines(reply):
    low = str(reply).lower()
    return bool(set(_WORD_RE.findall(low)) & _DECLINE_WORDS) \
        or bool(_DECLINE_PHRASE.search(low))


# Shown to the model when it answered a house-sounding request without using a
# tool. The second sentence matters: not every house-worded line is about the
# house ("a joke about a lamp"), and forcing a tool over those does harm.
GUARD_REPROMPT_TEXT = ("You did not call a tool. If this request changes or"
                       " reads the house, call the right tool now instead of"
                       " answering from memory; if it is not about the house,"
                       " just answer plainly.")


# --------------------------------------------------------------------------
# finding the house's own names inside a spoken sentence: a control verb plus
# a named target is the only thing worth forcing a tool over.

def _norm_words(text):
    """Lower-case text with accents and punctuation folded away, so a device
    or area label from the house list can be looked for inside a sentence."""
    s = unicodedata.normalize("NFKD", str(text).lower())
    s = "".join(ch for ch in s if not unicodedata.combining(ch))
    return " ".join(re.sub(r"[^a-z0-9']+", " ", s).split())


def _names_house_item(text, ctx):
    """True when the sentence contains an exposed area or device name this
    speaker was actually told about (name list from the turn context)."""
    t = _norm_words(text)
    if not t:
        return False
    for area, entry in ((ctx or {}).get("areas") or {}).items():
        entry = entry or {}
        for word in [area] + list(entry.get("lights") or []) \
                 + list(entry.get("climates") or []):
            name = _norm_words(word)
            if len(name) >= 3 and name in t:
                return True
    return False


def _forceable(text, ctx):
    """Only demand a forced tool call when the sentence both uses a control
    verb and names something to act on - a house object word or one of the
    exposed names. A joke about a lamp is not an order for a lamp."""
    toks = set(_WORD_RE.findall(str(text).lower()))
    if not (toks & _CONTROL_ACTIONS):
        return False
    return bool(toks & _CONTROL_OBJECTS) or _names_house_item(text, ctx)

# The tool the model uses to hand a personal or long request to Helm. This
# module answers it itself; the house-control tools come from homecontrol.
ASK_HELM_SCHEMA = {
    "type": "function",
    "function": {
        "name": "ask_helm",
        "description": ("Ask Helm for something this assistant cannot do: personal"
                        " things, calendar, reminders, money, research, or any long"
                        " answer. The short spoken part comes back here; the detail"
                        " goes to their phone."),
        "parameters": {
            "type": "object",
            "properties": {
                "text": {"type": "string",
                         "description": "the request, rephrased for Helm"}},
            "required": ["text"],
        },
    },
}

PROMPT_VERSION = "lean-v2"

# The whole persona and behaviour rules. Deliberately tiny: a small model
# follows short instructions better than long ones. A test pins this under
# 250 estimated tokens; the assembled prompt stays inside its budget with
# context and the four house tools (see the fixture budget test).
SYSTEM_PROMPT = """You are the house voice assistant. Answer by speech only.

Rules:
- Answer first, in one to three short spoken sentences. Plain words. No markdown, no lists, no urls, no entity ids. Say numbers as a person would.
- Ask a question back only when you truly cannot act without it.
- Anything about the house (lights, heating, timers, what is on): use the tools. Never claim you did something a tool did not do.
- "The [room] lights" means every light of that room: one call for the room, never one bulb at a time.
- Jokes and small talk: answer briefly yourself; never say you cannot.
- What chores or plants are due: call home_status. It reads only: never mark anything done by voice.
- If no room is named in a request, use the asking room.
- If a tool fails or has notes, say in one short sentence what worked and which devices could not."""

# Only added when an ask_helm tool was actually handed to the model; with no
# hub configured the tool and this line are both absent, so the model never
# reaches for something that would only speak an error.
ASK_HELM_RULE = ("- My calendar, my messages, reminders, money, or anything"
                 " that needs a long answer: call ask_helm. Do not guess at"
                 " those yourself.")


def estimate_tokens(text):
    """Rough token count used only by the size guards (four chars ~ one token)."""
    return max(1, len(text) // 4)


# --------------------------------------------------------------------------
# config and result

@dataclass
class Config:
    model_url: str
    model_name: str
    api_key: str = None
    extra_headers: dict = field(default_factory=dict)
    extra_body: dict = field(default_factory=dict)
    thinking: bool = True
    max_tokens: int = 2048
    temperature: float = 0.3
    timeout_s: float = 30.0
    history_turns: int = 2
    history_ttl_s: float = 120.0
    # rounds are model calls; several tool calls may share one round, and the
    # home-control gate can spend one extra round forcing a tool - four keeps
    # a whole-room command inside one spoken turn with room to spare
    max_tool_rounds: int = 4
    escalate_url: str = None
    escalate_model: str = None
    record_text: bool = True


@dataclass
class TurnResult:
    text: str                      # what to speak (fixed sentence on failures)
    kind: str                      # answer | escalate | offline | error | timeout
    tool_calls: list = field(default_factory=list)   # [{name, args, kind, ok, ms}]
    timings: dict = field(default_factory=dict)      # prompt_ms, model_ms[], tool_ms[], total_ms
    model_used: str = None         # model name; "escalate" after the retry
    prompt_hash: str = None        # sha256 over version + system text + tool defs
    tags: dict = None              # caller's labels, carried through verbatim
    escalate_text: str = None      # set when kind == escalate: what to ask Helm
    empty_final: bool = False      # tools worked but the model said nothing;
                                   # we spoke the tool's own sentence instead
    guard_reprompt: bool = False   # a house request answered with no tool call
                                   # was sent back once with the tool forced
    guard_blocked: bool = False    # what we spoke was replaced or joined by
                                   # our own honest failure words


# --------------------------------------------------------------------------
# reply cleaning

_THINK_BLOCK = re.compile(r"<(?:think|thinking|thought)>.*?</(?:think|thinking|thought)>",
                          re.IGNORECASE | re.DOTALL)
_UNCLOSED_THINK = re.compile(r"<(?:think|thinking|thought)\b[^>]*>.*\Z",
                             re.IGNORECASE | re.DOTALL)


def strip_thinking(text):
    """Remove closed thinking blocks; when a think block was never closed the
    rest of the reply is reasoning, so nothing after its opening tag stays."""
    if text is None:
        return ""
    text = _THINK_BLOCK.sub(" ", text)
    return _UNCLOSED_THINK.sub(" ", text).strip()


def clean_reply(text):
    """Make model output speakable: drop markup, lists and urls, then cut at
    a sentence boundary near 300 characters."""
    if not isinstance(text, str):
        return ""
    t = strip_thinking(text)
    # a stray thinking close tag (an opener the block patterns never saw)
    # must not reach the speaker: small models emit "</think" mid-sentence,
    # usually without even the closing bracket
    t = re.sub(r"</\s*th(?:ink|inking|ought)\w*\b>?", " ", t, flags=re.I)
    t = re.sub(r"```.*?```", " ", t, flags=re.DOTALL)          # fenced code
    t = re.sub(r"\[([^\]]*)\]\([^)]*\)", r"\1", t)             # markdown links
    t = re.sub(r"[*_`>#]+", " ", t)                            # emphasis/headers
    t = re.sub(r"^\s*(?:[-*+]|\d+[.)])\s+", "", t, flags=re.MULTILINE)  # lists
    t = re.sub(r"https?://\S+|www\.\S+", "", t)                # bare urls
    t = re.sub(r"\s+", " ", t).strip()
    if len(t) <= 300:
        return t
    window = t[:320]
    cut = -1
    for m in re.finditer(r"[.!?](?=\s|$)", window):
        if m.end() <= 300:
            cut = m.end()
    if cut == -1:
        space = window.rfind(" ", 200, 305)
        cut = space if space != -1 else 300
        return t[:cut].rstrip() + "..."
    return window[:cut].strip()


def ends_with_question(text):
    """Same rule as hubapi: HA keeps the microphone open only for replies that
    plainly ask something back."""
    return bool(text) and text.rstrip().endswith("?")


# --------------------------------------------------------------------------
# lenient tool-call parsing (small models are messy emitters)

def parse_tool_args(raw):
    """Parse a tool call's arguments leniently. Accepts a dict, a JSON object,
    JSON double-wrapped in a string, or an object with stray text around it.
    Raises ValueError when nothing usable is there."""
    if raw is None:
        return {}
    if isinstance(raw, dict):
        return raw
    if isinstance(raw, bool) or not isinstance(raw, str):
        raise ValueError("arguments must be a mapping or JSON text")
    s = raw.strip()
    try:
        obj = json.loads(s)
        if isinstance(obj, str):  # double-encoded: a string holding JSON
            obj = json.loads(obj)
    except ValueError:
        start, end = s.find("{"), s.rfind("}")
        if start == -1 or end <= start:
            raise ValueError("no JSON object found in arguments")
        try:
            obj = json.loads(s[start:end + 1])
        except ValueError as exc:
            raise ValueError("arguments are not valid JSON") from exc
    if not isinstance(obj, dict):
        raise ValueError("tool arguments must be a JSON object")
    return obj


_TOOL_BLOCK = re.compile(r"<tool_call(?:\s[^>]*)?>", re.IGNORECASE)
_TOOL_END = "</tool_call>"


def salvage_tool_calls(content):
    """Some chat templates put tool calls in content as a <tool_call> block
    instead of the structured field. Return (kept_text, [{name, arguments}])."""
    if not isinstance(content, str) or "<tool" not in content.lower():
        return content, []
    calls = []
    text_parts = []
    pos = 0
    while True:
        m = _TOOL_BLOCK.search(content, pos)
        if m is None:
            text_parts.append(content[pos:])
            break
        text_parts.append(content[pos:m.start()])
        end = content.find(_TOOL_END, m.end())
        tail = len(content) if end == -1 else end + len(_TOOL_END)
        payload = content[m.end():end if end != -1 else len(content)].strip()
        call = _parse_tool_payload(payload)
        if call is not None:
            calls.append(call)
        pos = tail
    return " ".join(p for p in text_parts if p.strip()), calls


def _parse_tool_payload(payload):
    """One tool-call block body: either JSON ({name, arguments}) or a call
    written as name({args})."""
    try:
        obj = json.loads(payload)
    except ValueError:
        obj = None
    if isinstance(obj, dict):
        name = obj.get("name") or obj.get("function")
        args = obj.get("arguments", obj.get("parameters", {}))
        if isinstance(name, str) and name.strip():
            return {"name": name.strip(), "arguments": args}
        return None
    m = re.match(r"([A-Za-z_][A-Za-z0-9_]*)\s*\((.*)\)\s*\Z", payload, re.DOTALL)
    if m:
        return {"name": m.group(1), "arguments": m.group(2)}
    return None


# --------------------------------------------------------------------------
# context assembly

def _now_text(ctx, clock):
    explicit = ctx.get("now")
    if isinstance(explicit, str) and explicit.strip():
        return explicit.strip()
    return time.strftime("%Y-%m-%d %H:%M (%A)", time.localtime(clock()))


PROMPT_NAME_CAP = 8       # names per area per kind in the prompt line
PROMPT_NAME_LEN = 40      # a renamed device never makes the prompt long


def _safe_name(value):
    """One house name destined for the prompt: no newlines, capped length.

    Entity names arrive json.dumps-escaped; room and area labels do not, so
    they are flattened here - a guest who renames a device cannot inject
    lines into our own rule text."""
    return " ".join(str(value).split())[:PROMPT_NAME_LEN]


def _fmt_names(names):
    """A list of house names for the prompt line. Plain words are written
    bare; only names that would read ambiguously get quoted. json.dumps on
    every name costs a real token per light, and a forty-light house pays it."""
    parts = [n if not any(c in n for c in ',;:"') else json.dumps(n)
             for n in names]
    return "[" + ", ".join(parts) + "]"


def build_context(ctx, cfg, clock=time.time):
    """The minimised per-turn context: date/time, asking room, one compact line
    per area listing only exposed light and climate names (capped, with a
    "(+N more)" tail). No entity ids, no states, no attributes - the live house
    exposes about forty speakable lights and most are noise for speech. The
    "Unassigned" bucket never shows: it is noise as a spoken target, while the
    tools still resolve those names. History is added separately."""
    ctx = ctx or {}
    lines = ["Time: " + _now_text(ctx, clock)]
    room = _safe_name(ctx.get("room") or "")
    if room:
        lines.append("Asking room: " + room)
    areas = ctx.get("areas") or {}
    for name in sorted(areas, key=str):
        label = _safe_name(name)
        if not label or label.lower() == "unassigned":
            continue
        entry = areas[name] or {}
        parts = []
        for kind, word in (("lights", "lights"), ("climates", "heating")):
            names = sorted(n for n in (_safe_name(x)
                                       for x in (entry.get(kind) or [])) if n)
            if not names:
                continue
            shown = names[:PROMPT_NAME_CAP]
            tail = len(names) - len(shown)
            parts.append("%s %s%s" % (word, _fmt_names(shown),
                                      " (+%d more)" % tail if tail else ""))
        if parts:
            lines.append("%s: %s" % (label, "; ".join(parts)))
    lines.append("If the house is not named here, it is hidden from voice.")
    return "\n".join(lines)


def _history_messages(ctx, cfg, clock=time.time):
    """Only the last history_turns exchanges, and only inside history_ttl_s:
    older exchanges are stale conversation for a voice command."""
    out = []
    for item in (ctx or {}).get("history") or []:
        if not isinstance(item, dict):
            continue
        user, spoken = item.get("user"), item.get("spoken")
        ts = item.get("ts")
        if not (isinstance(user, str) and user.strip()):
            continue
        if isinstance(ts, (int, float)) and clock() - ts > cfg.history_ttl_s:
            continue
        out.append({"role": "user", "content": user.strip()})
        if isinstance(spoken, str) and spoken.strip():
            out.append({"role": "assistant", "content": spoken.strip()})
    keep = max(0, int(cfg.history_turns)) * 2
    return out[-keep:]


def prompt_hash(system_text, schemas):
    """Stable fingerprint of exactly what the model was told (version + rules +
    context + tool defs); it rides in every timing record."""
    blob = "\x00".join([PROMPT_VERSION, system_text,
                        json.dumps(schemas, sort_keys=True, separators=(",", ":"))])
    return hashlib.sha256(blob.encode("utf-8")).hexdigest()[:16]


# --------------------------------------------------------------------------
# model client

def request_body(cfg, messages, schemas, model=None, tool_choice="auto"):
    body = {
        "model": model or cfg.model_name,
        "messages": messages,
        "tool_choice": tool_choice,
        "max_tokens": int(cfg.max_tokens),
        "temperature": float(cfg.temperature),
    }
    if schemas:
        body["tools"] = schemas
    body.update(cfg.extra_body or {})  # template/thinking switches ride here
    return body


def _headers(cfg):
    headers = {"Content-Type": "application/json"}
    if cfg.api_key:
        headers["Authorization"] = "Bearer " + str(cfg.api_key)
    # extra_headers last: a priority-lane header is exactly what config adds.
    headers.update({str(k): str(v) for k, v in (cfg.extra_headers or {}).items()})
    return headers


async def _chat(cfg, transport, url, messages, schemas, model, timings,
                force_tools=False):
    """One chat-completions call; the outcome is a state string plus the first
    choice's pieces. Never raises: network pain becomes offline/timeout.
    force_tools=true backs the home-control gate with tool_choice=required."""
    choice = "required" if (force_tools and schemas) else "auto"
    data = json.dumps(request_body(cfg, messages, schemas, model,
                                   tool_choice=choice)).encode("utf-8")
    started = time.monotonic()
    try:
        status, raw = await transport.request(
            "POST", url, headers=_headers(cfg), data=data,
            timeout=cfg.timeout_s + 5)
    except (RequestTimeout, TimeoutError):
        # RequestTimeout first: it is a TransportError subclass, and a slow
        # model must be spoken as a timeout, never dressed up as offline
        timings["model_ms"].append(round((time.monotonic() - started) * 1000))
        return {"state": "timeout"}
    except TransportError:
        return {"state": "offline"}
    timings["model_ms"].append(round((time.monotonic() - started) * 1000))
    if status != 200:
        return {"state": "error"}
    try:
        payload = json.loads(raw.decode("utf-8"))
    except (ValueError, AttributeError, UnicodeDecodeError):
        return {"state": "error"}
    choices = payload.get("choices") if isinstance(payload, dict) else None
    message = finish = None
    if isinstance(choices, list) and choices and isinstance(choices[0], dict):
        message = choices[0].get("message")
        finish = choices[0].get("finish_reason")
    if not isinstance(message, dict):
        return {"state": "error"}
    calls = []
    for call in message.get("tool_calls") or []:
        if not isinstance(call, dict):
            continue
        fn = call.get("function") or {}
        name = fn.get("name") or call.get("name")
        if isinstance(name, str) and name.strip():
            calls.append({"name": name.strip(), "arguments": fn.get("arguments", {})})
    kept_text, salvaged = salvage_tool_calls(message.get("content"))
    return {"state": "ok", "content": kept_text, "tool_calls": calls + salvaged,
            "finish_reason": finish}


def _brief(outcome):
    """Trim a tool result to what the model needs; keeps the transcript small."""
    if not isinstance(outcome, dict):
        return {"ok": False, "error": "the tool returned no result"}
    out = {}
    for key in ("ok", "text", "error", "state", "brightness_pct", "temperature",
                "mode", "candidates", "message", "notes"):
        if key in outcome:
            value = outcome[key]
            if isinstance(value, str) and len(value) > 200:
                value = value[:200] + "..."
            out[key] = value
    return out or {"ok": bool(outcome.get("ok", False))}


# Our own wiring messages: true in the records, meaningless out loud. They
# never reach the speaker - an internal failure falls back to CONTROL_TEXT.
_INTERNAL_FAILURES = ("bad arguments:", "unknown tool:", "the light action",
                      "the climate action", "the timer action",
                      "home_status needs", "the tool itself failed")


def _speakable_failure(entry):
    """One recorded failed call in words a person may be told, or None when
    the reason is internal plumbing. An ambiguity error's candidate list rides
    along in brackets, so the spoken line can actually ask something useful."""
    raw = str(entry.get("error") or "")
    low = raw.strip().lower()
    if any(low.startswith(m) for m in _INTERNAL_FAILURES):
        return None
    reason = clean_reply(raw)
    if not reason or len(reason) > 160:
        return None
    cands = [str(c)[:40] for c in (entry.get("candidates") or [])[:4]]
    if cands:
        reason += " (%s)" % ", ".join(dict.fromkeys(cands))
    return reason


def _failure_sentence(collected):
    """One honest spoken line for a house turn where every tool call failed:
    the most recent plain-language reason from the tools themselves - with
    their offered candidates - or the fixed short sentence if nothing
    speakable is there."""
    for entry in reversed(collected or []):
        reason = _speakable_failure(entry)
        if reason:
            return reason
    return CONTROL_TEXT


# --------------------------------------------------------------------------
# the turn

async def run_turn(cfg, transport, tools, ctx, text, tags=None, clock=time.time):
    """One spoken turn: lean prompt, model calls (up to max_tool_rounds rounds
    of house tools), one TurnResult.

    tools duck-types homecontrol: .schemas -> OpenAI function defs without
    ask_helm; await .execute(name, args) -> plain dict with an "ok" bool.

    Home-control gate: when the request words name a house object or action,
    words may not be spoken over nothing - a write request needs a successful
    WRITE call standing behind it (a read proves only that a read worked). A
    single clean question claiming no success is always allowed through. A
    tool-less claim is re-prompted once with the tool forced; if even then no
    tool runs, the fixed CONTROL_TEXT goes instead of any success claim. If
    every tool call failed, the honest sentence is built from the tools' own
    reasons and candidates, never from the model's wording; when only part of
    the ask worked, the failures are appended to the model's own sentence.

    If the model calls ask_helm, or its answer is unusable (empty, invalid tool
    calls twice, it kept calling tools past the budget, timed out or could not
    be reached) and an escalation endpoint is configured, retry once there -
    recorded as model_used = "escalate". The hub path itself belongs to the
    caller: kind escalate carries escalate_text.
    """
    started = time.monotonic()
    timings = {"prompt_ms": 0, "model_ms": [], "tool_ms": [], "total_ms": 0}

    house_schemas = list(getattr(tools, "schemas", None) or [])
    # a hub-less entry hands over no ask_helm tool and says nothing about it:
    # the model then answers or declines by itself instead of speaking an error
    ask_enabled = bool(getattr(tools, "ask_enabled", True))
    schemas = house_schemas + ([ASK_HELM_SCHEMA] if ask_enabled else [])
    known_names = set()
    for schema in house_schemas:
        fn = schema.get("function") if isinstance(schema, dict) else None
        if isinstance(fn, dict) and isinstance(fn.get("name"), str):
            known_names.add(fn["name"])

    t_prompt = time.monotonic()
    system_text = build_context(ctx, cfg, clock)
    rules = SYSTEM_PROMPT + ("\n" + ASK_HELM_RULE if ask_enabled else "")
    timings["prompt_ms"] = round((time.monotonic() - t_prompt) * 1000)
    p_hash = prompt_hash(rules + "\n\n" + system_text, schemas)
    base_messages = (
        [{"role": "system", "content": rules + "\n\n" + system_text}]
        + _history_messages(ctx, cfg, clock)
        + [{"role": "user", "content": str(text)}])

    collected = []  # every tool attempt this turn: {name, args, kind, ok, ms[...]}
    control_ask = is_home_control(text)
    want_write = bool(set(_WORD_RE.findall(str(text).lower())) & _WRITE_VERBS)
    # a forced tool round only makes sense when the sentence names a target
    can_force = _forceable(text, ctx)
    guard_state = {"reprompt": False, "blocked": False}

    def result(kind, sentence, escalate_text=None, model_used=cfg.model_name,
               empty_final=False):
        timings["total_ms"] = round((time.monotonic() - started) * 1000)
        return TurnResult(text=sentence, kind=kind, tool_calls=collected,
                          timings=timings, model_used=model_used,
                          prompt_hash=p_hash, tags=dict(tags or {}),
                          escalate_text=escalate_text, empty_final=empty_final,
                          guard_reprompt=guard_state["reprompt"],
                          guard_blocked=guard_state["blocked"])

    def give_up(state, used):
        sentence = {KIND_OFFLINE: OFFLINE_TEXT, KIND_TIMEOUT: TIMEOUT_TEXT,
                    "empty": EMPTY_TEXT}.get(state["state"], ERROR_TEXT)
        kind = (state["state"] if state["state"] in (KIND_OFFLINE, KIND_TIMEOUT)
                else KIND_ERROR)
        return result(kind, sentence, model_used=used)

    async def attempt(url, model_name, guarded=True):
        """Work one endpoint until it answers or dies. Returns a state dict:
        answer | escalate | empty | unreliable | rounds | guard_none |
        offline | timeout | error. ``guarded`` is False for the escalation
        endpoint only: that second brain re-derives its own answer (and in a
        real entry escalate_url is unset, so house words run guarded once)."""
        msgs = [dict(m) for m in base_messages]
        bad_rounds = 0
        capped = False
        last_ok_text = None  # the sentence the last succeeding tool returned
        force_tools = False  # set by the gate: next call demands a tool
        reprompted = False

        def carried():
            """Did a tool really carry this turn? A write request needs a
            successful WRITE - a home_state read that happened to work proves
            nothing about the change that was asked for."""
            ok_calls = [c for c in collected if c["ok"]]
            return bool(ok_calls) and (not want_write or any(
                c.get("kind") == "write" for c in ok_calls))

        def gate(reply, raw_content, round_no):
            """Words are about to be spoken for a house request. None lets
            them through; otherwise returns the state that must go instead -
            {"state": "retry"} means we appended the forced-tool prompt."""
            nonlocal force_tools, reprompted
            if not (guarded and control_ask):
                return None
            if carried():
                failed = [c for c in collected if not c["ok"]]
                if failed:
                    # partial success: the model's sentence may ride, but a
                    # failed part of the ask is appended in the tools' own
                    # words - "kitchen and bedroom are off" must not hide
                    # that the bedroom call never went through
                    notes = [n for n in (_speakable_failure(c) for c in failed)
                             if n and n.lower() not in reply.lower()]
                    if notes:
                        guard_state["blocked"] = True  # our words joined in
                        joiner = " " if reply[-1] in ".!?" else ". "
                        return {"state": "answer",
                                "reply": clean_reply(
                                    reply + joiner + "Not done: "
                                    + "; ".join(dict.fromkeys(notes)))}
                return None  # a tool really did carry this turn
            if _pure_question(reply):
                # an honest ask-back claims no success - and when every call
                # failed with its own "which one?" the model's fuller wording
                # of that question is the most useful line there is
                return None
            if collected and not any(c["ok"] for c in collected):
                # tools ran and every one failed: our own sentence from the
                # tools' reasons, never the model's claim of success
                guard_state["blocked"] = True
                return {"state": "answer",
                        "reply": _failure_sentence(collected)}
            if _declines(reply):
                # an honest "sorry, I didn't catch that" stands as spoken:
                # forcing a tool over a mishearing would act on the guess
                return None
            claim = _is_claim(reply)
            if not (claim or can_force):
                # an ordinary answer to house-worded words that are not about
                # doing anything in the house - a joke about a lamp, a
                # reminder for mum, the hour the shop opens: the gate only
                # guards actions on something the sentence names
                return None
            # nothing has run at all, or only reads worked for an ask that
            # wanted a change: those words claim something nobody did
            if not reprompted and schemas \
                    and round_no < int(cfg.max_tool_rounds):
                msgs.append({"role": "assistant",
                             "content": raw_content or None})
                msgs.append({"role": "user",
                             "content": GUARD_REPROMPT_TEXT})
                reprompted = True
                force_tools = True
                guard_state["reprompt"] = True
                return {"state": "retry"}
            guard_state["blocked"] = True
            return {"state": "guard_none"}

        for round_no in range(int(cfg.max_tool_rounds) + 1):
            resp = await _chat(cfg, transport, url, msgs, schemas, model_name,
                               timings, force_tools=force_tools)
            force_tools = False
            if resp["state"] in ("offline", "timeout"):
                return {"state": resp["state"]}
            if resp["state"] != "ok":
                return {"state": "error"}
            content = resp.get("content") or ""
            if resp.get("finish_reason") == "length":
                capped = True
            entries = []  # (name, args-or-None, error-message-or-None)
            for call in resp.get("tool_calls") or []:
                try:
                    entries.append((call["name"],
                                    parse_tool_args(call.get("arguments")), None))
                except ValueError as exc:
                    entries.append((call["name"], None, str(exc)))

            ask = next((e for e in entries if e[0] == "ask_helm" and e[1] is not None),
                       None)
            if ask is not None:
                asked = ask[1].get("text")
                return {"state": KIND_ESCALATE,
                        "text": (asked.strip() if isinstance(asked, str)
                                 and asked.strip() else str(text))}

            house = [e for e in entries if e[0] != "ask_helm"]
            if not house:
                reply = clean_reply(content)
                if reply:
                    override = gate(reply, content, round_no)
                    if override is not None:
                        if override["state"] == "retry":
                            continue
                        return override
                    return {"state": "answer", "reply": reply}
                if carried():
                    # The house action already succeeded; a model that then
                    # says nothing (thinking ate the token budget) must not
                    # deny something that worked. Speak the tool's own line.
                    spoken = clean_reply(last_ok_text or "") or "Done."
                    return {"state": "answer", "reply": spoken,
                            "empty_final": True}
                # empty answer (possibly only thinking): retrying locally is
                # exactly what the escalation endpoint is for; do not spin.
                return {"state": "empty", "capped": capped}

            if round_no >= int(cfg.max_tool_rounds):
                # still calling tools past the budget: judge any text once and
                # stop rather than looping forever.
                reply = clean_reply(content)
                if not reply and carried():
                    spoken = clean_reply(last_ok_text or "") or "Done."
                    return {"state": "answer", "reply": spoken,
                            "empty_final": True}
                if reply:
                    # gate never retries past the cap - at this round a house
                    # request with no worked tool is blocked outright
                    override = gate(reply, content, round_no)
                    if override is not None and override["state"] != "retry":
                        return override
                return ({"state": "answer", "reply": reply} if reply
                        else {"state": "rounds"})
            assistant_calls, tool_msgs, was_bad = [], [], False
            for idx, (name, args, err) in enumerate(house):
                call_id = "c%d-%d" % (round_no, idx)
                assistant_calls.append({"id": call_id, "type": "function",
                                        "function": {"name": name,
                                                     "arguments": json.dumps(args or {})}})
                if err is not None:
                    was_bad = True
                    collected.append({"name": name, "args": {},
                                      "kind": _call_kind(name, {}),
                                      "ok": False, "ms": 0,
                                      "error": "bad arguments: " + err[:150]})
                    tool_msgs.append({"role": "tool", "tool_call_id": call_id,
                                      "content": json.dumps(
                                          {"ok": False, "error": "bad arguments: " + err})})
                elif name not in known_names:
                    was_bad = True
                    collected.append({"name": name, "args": args,
                                      "kind": _call_kind(name, args),
                                      "ok": False, "ms": 0,
                                      "error": "unknown tool: " + name})
                    tool_msgs.append({"role": "tool", "tool_call_id": call_id,
                                      "content": json.dumps(
                                          {"ok": False, "error": "unknown tool: " + name})})
                else:
                    t_tool = time.monotonic()
                    try:
                        outcome = await tools.execute(name, args)
                    except Exception:
                        # a raising tool is a bug in our house code; the turn
                        # must still survive and speak a plain sentence
                        outcome = {"ok": False, "error": "the tool itself failed"}
                    ms = round((time.monotonic() - t_tool) * 1000)
                    ok_now = bool(isinstance(outcome, dict)
                                  and outcome.get("ok") is True)
                    if ok_now and isinstance(outcome.get("text"), str) \
                            and outcome["text"].strip():
                        last_ok_text = outcome["text"]
                    entry = {"name": name, "args": args,
                             "kind": _call_kind(name, args),
                             "ok": ok_now, "ms": ms}
                    if not ok_now:
                        # kept so a failed house turn can be spoken honestly
                        # from the tool's own reason rather than invented
                        why = outcome.get("error") if isinstance(outcome, dict) \
                            else None
                        entry["error"] = str(why or "the call did not go through")[:200]
                        cands = outcome.get("candidates") \
                            if isinstance(outcome, dict) else None
                        if isinstance(cands, list) and cands:
                            # an ambiguity ask-back is only useful out loud
                            # with the options that came back with it
                            entry["candidates"] = [str(x)[:60]
                                                   for x in cands[:8]]
                    collected.append(entry)
                    timings["tool_ms"].append(ms)
                    tool_msgs.append({"role": "tool", "tool_call_id": call_id,
                                      "content": json.dumps(_brief(outcome))})
            msgs.append({"role": "assistant", "content": content or None,
                         "tool_calls": assistant_calls})
            msgs.extend(tool_msgs)
            bad_rounds += 1 if was_bad else 0
            if bad_rounds >= 2:
                return {"state": "unreliable"}
        return {"state": "rounds"}

    state = await attempt(cfg.model_url, cfg.model_name, guarded=True)
    if state["state"] == "answer":
        return result(KIND_ANSWER, state["reply"], model_used=cfg.model_name,
                      empty_final=bool(state.get("empty_final")))
    if state["state"] == KIND_ESCALATE:
        return result(KIND_ESCALATE, "", escalate_text=state["text"],
                      model_used=cfg.model_name)
    if state["state"] == "guard_none":
        # a house request the model refused to carry out with tools even when
        # forced: the gate outranks the endpoint retry - no words that could
        # claim a success nobody performed, from either endpoint this turn
        return result(KIND_ERROR, CONTROL_TEXT)

    # Unusable on the main endpoint (empty, unreliable, out of rounds - which
    # includes thinking that stopped at the token cap without answering - or a
    # dead endpoint): exactly one retry on the escalation endpoint. This is the
    # benchmark's "the 4B gets it wrong, Flash picks it up" case.
    if cfg.escalate_url:
        fb_name = cfg.escalate_model or cfg.model_name
        state = await attempt(cfg.escalate_url, fb_name, guarded=False)
        if state["state"] == "answer":
            return result(KIND_ANSWER, state["reply"], model_used="escalate",
                          empty_final=bool(state.get("empty_final")))
        if state["state"] == KIND_ESCALATE:
            return result(KIND_ESCALATE, "", escalate_text=state["text"],
                          model_used="escalate")
        return give_up(state, "escalate")

    return give_up(state, cfg.model_name)
