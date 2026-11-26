"""Replay a recorded set of requests through the production agent.

Two doors, one engine: outside Home Assistant ``python replay.py`` runs a
JSONL set against configured model endpoints directly (tools in dry mode;
calls are recorded, the house is untouched); inside HA the
``helm_speaker.replay`` service feeds the same rows through
``conversation.process``, so the live entry's profiles, records and timing
lines all apply.

Rows: {"id": "...", "text"?: str, "audio"?: wav-path, "expect"?: {...}}.
Text goes straight to the agent; audio is POSTed to a real whisper-http
endpoint (/v1/audio/transcriptions) first, and that call is timed.
Nothing in here starts or resizes a model - endpoints are config values.

Expect matching: {"tool": name, "args": {...}} matches when one recorded
tool call carries that name and its args contain every key/value listed
(subset match on scalars). The verdict lands in each timing record as
"expect_match".
"""

import asyncio
import json
import time
import urllib.request
import uuid

try:  # inside Home Assistant (or the test package)
    from . import agent_core
    from .agent_core import Config
    from .homecontrol import HomeControl
    from .stages import StageLog
    from .timings import TimingWriter
except ImportError:  # run as a plain script from this folder
    import importlib
    import os
    import sys as _sys
    import types
    if "hs" not in _sys.modules:
        _pkg = types.ModuleType("hs")
        _pkg.__path__ = [os.path.dirname(os.path.abspath(__file__))]
        _sys.modules["hs"] = _pkg
    agent_core = importlib.import_module("hs.agent_core")
    Config = agent_core.Config
    HomeControl = importlib.import_module("hs.homecontrol").HomeControl
    StageLog = importlib.import_module("hs.stages").StageLog
    TimingWriter = importlib.import_module("hs.timings").TimingWriter


class EmptyBackend:
    """No house visible (text-only replay without --house): everything is
    unknown to the tools, which then answer honestly."""

    async def exposed(self):
        return {}

    async def states(self, entity_ids):
        return {}

    async def call_service(self, domain, service, data):
        raise RuntimeError("replay has no house; run with --house or dry")

    async def intent(self, name, data):
        return {"speech": None}


class CliBackend:
    """A house from a JSON file for CLI replay: exposed lists and (optionally)
    states/attributes, so name resolution and status tools work. Used dry by
    default; act=True is refused on purpose - only HA may touch the house."""

    def __init__(self, house):
        self._areas = {}
        for area, kinds in (house or {}).items():
            entry = {"lights": [], "climates": []}
            for kind in ("lights", "climates"):
                for item in kinds.get(kind) or []:
                    name, eid = item[0], item[1]
                    entry[kind].append((name, eid))
            self._areas[area] = entry
        self._states = {eid: dict(attrs)
                        for attrs in (house or {}).get("_states") or []
                        for eid in [attrs.get("entity_id")] if eid}

    async def exposed(self):
        return self._areas

    async def states(self, entity_ids):
        return {eid: self._states[eid] for eid in entity_ids
                if eid in self._states}

    async def call_service(self, domain, service, data):
        raise RuntimeError("CLI replay never operates the house (dry only)")

    async def intent(self, name, data):
        return {"speech": None}


def load_set(path_or_lines):
    if isinstance(path_or_lines, str):
        with open(path_or_lines, encoding="utf-8") as fh:
            lines = fh
    else:
        lines = path_or_lines
    rows = []
    for line in lines:
        line = line.strip()
        if line:
            row = json.loads(line)
            row.setdefault("id", uuid.uuid4().hex[:8])
            rows.append(row)
    return rows


def expect_match(row, result):
    want = row.get("expect")
    if not want:
        return None
    name = want.get("tool")
    args_want = want.get("args") or {}
    for call in result.tool_calls or []:
        if call.get("name") != name:
            continue
        args = call.get("args") or {}
        if all(args.get(k) == v for k, v in args_want.items()):
            return True
    return False


def whisper_transcribe(url, wav_bytes, timeout=30):
    """POST one wav to a whisper-http endpoint; returns (text, seconds)."""
    boundary = "helmspk"
    body = (
        ("--" + boundary + "\r\n"
         'Content-Disposition: form-data; name="file"; filename="a.wav"\r\n'
         "Content-Type: audio/wav\r\n\r\n").encode()
        + wav_bytes
        + ("\r\n--" + boundary + "--\r\n").encode()
    )
    req = urllib.request.Request(
        url, data=body, method="POST",
        headers={"Content-Type": "multipart/form-data; boundary=" + boundary})
    started = time.monotonic()
    with urllib.request.urlopen(req, timeout=timeout) as resp:  # noqa: S310 (plain LAN http by design)
        payload = json.loads(resp.read().decode("utf-8"))
    took = time.monotonic() - started
    return (payload.get("text") or "").strip(), took


def make_ctx(backend_areas):
    areas = {}
    for area, entry in (backend_areas or {}).items():
        areas[area] = {"lights": [n for n, _ in entry.get("lights") or []],
                       "climates": [n for n, _ in entry.get("climates") or []]}
    return {"room": None, "areas": areas, "history": []}


class UrllibTransport:
    """The small transport interface agent_core expects, over urllib, so
    replay runs without Home Assistant. Same contract as the aiohttp one."""

    async def request(self, method, url, *, headers=None, data=None,
                      timeout=None):
        def go():
            req = urllib.request.Request(url, data=data, method=method,
                                         headers=headers or {})
            try:
                with urllib.request.urlopen(req, timeout=timeout) as resp:  # noqa: S310
                    return resp.status, resp.read()
            except urllib.error.HTTPError as err:  # 4xx/5xx are answers, not crashes
                return err.code, err.read()
        try:
            return await asyncio.wait_for(asyncio.to_thread(go), timeout + 2)
        except (asyncio.TimeoutError, OSError, ValueError) as err:
            raise agent_core.TransportError(str(err)) from err


def config_from_args(args):
    """One profile straight from the command line."""
    headers = {}
    for kv in args.header or []:
        key, _, value = kv.partition("=")
        headers[key] = value
    body = json.loads(args.body_json) if args.body_json else {}
    return Config(model_url=args.model_url, model_name=args.model_name,
                  extra_headers=headers, extra_body=body,
                  thinking=not args.no_thinking, timeout_s=args.timeout)


async def run_set(rows, *, config, control, emit, transport=None, tags=None,
                  whisper_url=None, repeat=1, burst=1, gap_ms=0):
    """Run every row (repeat x, and each row burst times for queue tests).
    Returns the list of written records."""
    transport = transport or UrllibTransport()
    log = StageLog(emit)  # emit may be a TimingWriter or any .append() sink
    ctx = make_ctx(await control.backend.exposed())
    history = []
    written = []
    for _ in range(max(1, repeat)):
        for row in rows:
            text = row.get("text")
            stt = None
            if not text and row.get("audio") and whisper_url:
                with open(row["audio"], "rb") as fh:
                    wav = fh.read()
                text, took = await asyncio.to_thread(
                    whisper_transcribe, whisper_url, wav)
                now = time.time()
                stt = {"listen_start_ts": now - took, "collected_ts": now - took,
                       "done_ts": now, "listened_s": 0.0, "stt_ms": took}
            if not text:
                continue
            turn_tags = dict(tags or {})
            turn_tags.update(row.get("tags") or {})
            turn_tags.setdefault("set_id", row.get("id"))
            for _ in range(max(1, burst)):
                result = await agent_core.run_turn(
                    config, transport, control, ctx, text, tags=turn_tags,
                    clock=time.time)
                record = log.finish_turn(
                    turn_id=uuid.uuid4().hex, profile=config.model_name,
                    result=result, kind=result.kind, tags=turn_tags,
                    stt=stt, text_in=text, text_out=result.text)
                record["expect_match"] = expect_match(row, result)
                written.append(record)
                history.append({"user": text, "spoken": result.text,
                                "ts": time.time()})
                ctx["history"] = history[-4:]
                if gap_ms:
                    await asyncio.sleep(gap_ms / 1000.0)
    return written


def summarize(records):
    """Tiny per-tag summary: rows, mean model ms, expect-match rate."""
    groups = {}
    for rec in records:
        key = tuple(sorted((rec.get("tags") or {}).items()))
        groups.setdefault(key, []).append(rec)
    lines = []
    for key, recs in sorted(groups.items()):
        models = [(r.get("stage_ms") or {}).get("model") for r in recs]
        models = [m for m in models if m is not None]
        checks = [r.get("expect_match") for r in recs
                  if r.get("expect_match") is not None]
        lines.append({
            "tags": dict(key),
            "rows": len(recs),
            "model_ms_mean": round(sum(models) / len(models)) if models else None,
            "match": (f"{sum(1 for c in checks if c)}/{len(checks)}"
                      if checks else None),
        })
    return lines


def register_service(hass, entry):
    """helm_speaker.replay: run a JSONL set through conversation.process.

    Dry by default (entry.runtime_data["control_dry"]); act=true really
    operates the house and is meant for Ope-visible benchmark runs. Tags
    come from input_text.helm_speaker_tags (conversation reads them)."""
    import voluptuous as vol  # noqa: PLC0415 - HA-only path
    from homeassistant.core import ServiceCall  # noqa: PLC0415
    from homeassistant.helpers import config_validation as cv  # noqa: PLC0415

    schema = vol.Schema({
        vol.Required("path"): cv.string,
        vol.Optional("act", default=False): bool,
        vol.Optional("repeat", default=1): vol.All(int, vol.Range(min=1, max=20)),
        vol.Optional("burst", default=1): vol.All(int, vol.Range(min=1, max=50)),
        vol.Optional("gap_ms", default=0): vol.All(int, vol.Range(min=0, max=60000)),
    })

    async def handle(call: ServiceCall) -> None:
        path = hass.config.path(call.data["path"])
        rows = load_set(path)
        runtime = entry.runtime_data or {}
        runtime["control_dry"] = not call.data["act"]
        try:
            for _ in range(call.data["repeat"]):
                for row in rows:
                    text = row.get("text")
                    if not text:
                        continue  # audio rows belong to the CLI (whisper) path
                    for _ in range(call.data["burst"]):
                        await hass.services.async_call(
                            "conversation", "process",
                            {"text": text,
                             "agent_id": "conversation.helm_speaker"},
                            blocking=True)
                        if call.data["gap_ms"]:
                            await asyncio.sleep(call.data["gap_ms"] / 1000.0)
        finally:
            runtime["control_dry"] = False

    hass.services.async_register("helm_speaker", "replay", handle,
                                 schema=schema)


def main(argv=None):
    import argparse

    ap = argparse.ArgumentParser(
        description="Replay a request set through the production agent (dry "
                    "unless inside HA's helm_speaker.replay service).")
    ap.add_argument("--set", required=True, help="JSONL set file")
    ap.add_argument("--model-url", required=True)
    ap.add_argument("--model-name", required=True)
    ap.add_argument("--header", action="append", metavar="K=V",
                    help="extra header; repeatable (the priority lane lives here)")
    ap.add_argument("--body-json", help="extra request body as JSON")
    ap.add_argument("--no-thinking", action="store_true")
    ap.add_argument("--timeout", type=float, default=30.0)
    ap.add_argument("--house", help="JSON house file (names/entities per area)")
    ap.add_argument("--whisper-url", help="POST audio rows here first")
    ap.add_argument("--tag", action="append", metavar="K=V",
                    help="benchmark tag on every row; repeatable")
    ap.add_argument("--repeat", type=int, default=1)
    ap.add_argument("--burst", type=int, default=1)
    ap.add_argument("--gap-ms", type=int, default=0)
    ap.add_argument("--out", default="replay-timings.jsonl",
                    help="timing JSONL to append (same shape as the HA log)")
    args = ap.parse_args(argv)

    tags = {}
    for kv in args.tag or []:
        key, _, value = kv.partition("=")
        tags[key] = value

    house = None
    if args.house:
        with open(args.house, encoding="utf-8") as fh:
            house = json.load(fh)
    backend = CliBackend(house) if house else EmptyBackend()
    control = HomeControl(backend, dry=True)  # the CLI never touches the house
    rows = load_set(args.set)

    async def go():
        return await run_set(
            rows, config=config_from_args(args), control=control,
            emit=TimingWriter(args.out), tags=tags,
            whisper_url=args.whisper_url, repeat=args.repeat,
            burst=args.burst, gap_ms=args.gap_ms)

    written = asyncio.run(go())
    for line in summarize(written):
        print(json.dumps(line, sort_keys=True))


if __name__ == "__main__":
    main()
