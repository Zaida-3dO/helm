"""Tests for hubapi.py: the wire between the component and the hub.

Run with the plain standard library, no install step:

    cd tools/ha-speaker
    python -m unittest test_hubapi

A fake transport answers the hub's routes in memory; nothing here opens a
network socket. The Home Assistant glue itself (config flow, entities) is
not covered: it needs a running Home Assistant and is checked by hand.
"""

import json
import struct
import unittest
from urllib.parse import urlparse, parse_qs

import hubapi
from hubapi import (
    ERROR_TEXT,
    KIND_ANSWER,
    KIND_BAD_AUTH,
    KIND_EMPTY,
    KIND_ERROR,
    KIND_OFFLINE,
    KIND_OK,
    KIND_TIMEOUT,
    OFFLINE_TEXT,
    TIMEOUT_TEXT,
    HubApi,
    TransportError,
    build_presence,
    ends_with_question,
    pcm_cap,
    session_elevated,
    wav_from_pcm,
)


class FakeTransport:
    """Answers from a scripted queue: (status, dict-or-bytes) per call, or
    None to raise TransportError. Records every call."""

    def __init__(self, script=()):
        self.script = list(script)
        self.calls = []

    async def request(self, method, url, *, headers=None, data=None, timeout=None):
        self.calls.append(
            {"method": method, "url": url, "headers": headers or {},
             "data": data, "timeout": timeout})
        if not self.script:
            raise AssertionError("transport called more often than scripted")
        action = self.script.pop(0)
        if action is None:
            raise TransportError("connection refused")
        status, payload = action
        body = payload if isinstance(payload, bytes) else json.dumps(payload).encode()
        return status, body


def make_api(script=(), clock=None):
    transport = FakeTransport(script)
    api = HubApi("http://hub.example:8480", "tok-1", transport,
                 clock=clock or (lambda: 0.0))
    return api, transport


def query_of(call):
    return parse_qs(urlparse(call["url"]).query)


def body_of(call):
    return json.loads(call["data"].decode())


class StatusTable(unittest.IsolatedAsyncioTestCase):
    async def test_status(self):
        cases = [
            ("reachable", [(200, {"ok": True, "session": None})], KIND_OK),
            ("session present", [(200, {"ok": True,
                                        "session": {"id": "s1"}})], KIND_OK),
            ("bad token", [(401, b"nope")], KIND_BAD_AUTH),
            ("forbidden", [(403, b"nope")], KIND_BAD_AUTH),
            ("hub refused call", [(500, b"oops")], KIND_ERROR),
            ("junk body", [(200, b"not json")], KIND_ERROR),
            ("ok missing", [(200, {"hello": 1})], KIND_ERROR),
            ("unreachable", [None], KIND_OFFLINE),
        ]
        for name, script, want in cases:
            with self.subTest(name):
                api, _ = make_api(script)
                result = await api.status()
                self.assertEqual(result["kind"], want)


class TranscribeTable(unittest.IsolatedAsyncioTestCase):
    async def test_transcribe(self):
        cases = [
            ("words", [(200, {"text": "turn on the light"})], KIND_ANSWER,
             "turn on the light"),
            ("placeholder (a passcode)", [(200, {"text": "[unlock abc123]"})],
             KIND_ANSWER, "[unlock abc123]"),
            ("empty capture", [(200, {"state": "empty"})], KIND_EMPTY, None),
            ("speech model down", [(200, {"state": "error"})], KIND_ERROR, None),
            ("hub says offline", [(200, {"state": "offline"})], KIND_OFFLINE, None),
            ("http 503", [(503, b"down")], KIND_ERROR, None),
            ("bad token", [(401, b"")], KIND_BAD_AUTH, None),
            ("unreachable", [None], KIND_OFFLINE, None),
        ]
        for name, script, want, want_text in cases:
            with self.subTest(name):
                api, transport = make_api(script)
                result = await api.transcribe(b"RIFFfakedata", room="kitchen",
                                              hint="a")
                self.assertEqual(result["kind"], want)
                if want_text is not None:
                    self.assertEqual(result["text"], want_text)
                call = transport.calls[0]
                self.assertEqual(call["method"], "POST")
                self.assertTrue(call["url"].startswith(
                    "http://hub.example:8480/speaker/stt?"))
                self.assertEqual(call["headers"]["Authorization"], "Bearer tok-1")
                self.assertEqual(call["headers"]["Content-Type"], "audio/wav")
                params = query_of(call)
                self.assertEqual(params.get("room"), ["kitchen"])
                self.assertEqual(params.get("hint"), ["a"])


class TurnTable(unittest.IsolatedAsyncioTestCase):
    async def test_direct_answer(self):
        api, transport = make_api([(200, {"state": "answer", "text": "It is three o'clock.",
                                          "session": {"id": "s1"}})])
        result = await api.run_turn(text="what time", turn_id="t1",
                                    device="dev-9", room="kitchen", area="Kitchen",
                                    presence={"area": "Kitchen"})
        self.assertEqual(result["kind"], KIND_ANSWER)
        self.assertEqual(result["text"], "It is three o'clock.")
        self.assertEqual(result["session"], {"id": "s1"})
        body = body_of(transport.calls[0])
        self.assertEqual(body["turn_id"], "t1")
        self.assertEqual(body["text"], "what time")
        self.assertEqual(body["device"], "dev-9")
        self.assertEqual(body["room"], "kitchen")
        self.assertEqual(body["area"], "Kitchen")
        self.assertEqual(body["presence"], {"area": "Kitchen"})
        self.assertEqual(body["wait"], 25)

    async def test_working_then_poll(self):
        clock = iter([0.0, 0.0, 26.0, 26.0, 47.0, 47.0]).__next__
        api, transport = make_api(
            [(202, {"state": "working"}),
             (202, {"state": "working"}),
             (200, {"state": "answer", "text": "done"})],
            clock=clock)
        result = await api.run_turn(text="long job", turn_id="t2")
        self.assertEqual(result["kind"], KIND_ANSWER)
        self.assertEqual(result["text"], "done")
        polls = [c for c in transport.calls if c["method"] == "GET"]
        self.assertEqual(len(polls), 2)
        self.assertIn("/speaker/turn/t2", polls[0]["url"])
        waits = [int(query_of(c)["wait"][0]) for c in polls]
        self.assertEqual(waits, [20, 20])

    async def test_budget_runs_out(self):
        # elapsed at each clock read: start, then three polls at 0, 30 and
        # 60 seconds; the fourth read (95 s) is past the 90 s budget, so the
        # component stops asking without spending a fourth poll request.
        times = iter([0.0, 0.0, 30.0, 60.0, 95.0])
        api, transport = make_api(
            [(202, {"state": "working"}), (202, {"state": "working"}),
             (202, {"state": "working"}), (202, {"state": "working"})],
            clock=lambda: next(times))
        result = await api.run_turn(text="slow", turn_id="t3", budget=90)
        self.assertEqual(result["kind"], KIND_TIMEOUT)
        polls = [c for c in transport.calls if c["method"] == "GET"]
        self.assertEqual(len(polls), 3)
        waits = [int(query_of(c)["wait"][0]) for c in polls]
        self.assertEqual(waits, [20, 20, 20])

    async def test_other_outcomes(self):
        cases = [
            ("hub offline answer", [(200, {"state": "offline"})], KIND_OFFLINE),
            ("working state on a 200", [(200, {"state": "working"})], KIND_ERROR),
            ("unknown state", [(200, {"state": "banana"})], KIND_ERROR),
            ("answer without text", [(200, {"state": "answer"})], KIND_ERROR),
            ("poll lost the turn", [(202, {"state": "working"}), (404, b"")],
             KIND_ERROR),
            ("poll refused", [(202, {"state": "working"}), (401, b"")], KIND_ERROR),
            ("unreachable mid-poll", [(202, {"state": "working"}), None],
             KIND_OFFLINE),
            ("http error on post", [(500, b"x")], KIND_ERROR),
            ("bad token", [(401, b"x")], KIND_ERROR),
            ("unreachable on post", [None], KIND_OFFLINE),
        ]
        for name, script, want in cases:
            with self.subTest(name):
                times = iter([0.0, 0.0, 5.0, 5.0])
                api, _ = make_api(script, clock=lambda: next(times, 5.0))
                result = await api.run_turn(text="x", turn_id="t")
                self.assertEqual(result["kind"], want)


class WavTable(unittest.TestCase):
    def test_header(self):
        pcm = b"\x01\x02" * 100
        wav = wav_from_pcm(pcm)
        self.assertEqual(wav[:4], b"RIFF")
        self.assertEqual(wav[8:12], b"WAVE")
        riff_size = struct.unpack("<I", wav[4:8])[0]
        self.assertEqual(riff_size, 36 + len(pcm))
        fmt = struct.unpack("<IHHIIHH", wav[16:36])
        self.assertEqual(fmt, (16, 1, 1, 16000, 32000, 2, 16))
        self.assertEqual(wav[36:40], b"data")
        self.assertEqual(struct.unpack("<I", wav[40:44])[0], len(pcm))
        self.assertEqual(wav[44:], pcm)

    def test_cap(self):
        self.assertEqual(pcm_cap(16000, 30), 16000 * 2 * 30)


class SmallHelpersTable(unittest.TestCase):
    def test_questions(self):
        cases = [
            ("What is the time?", True),
            ("Sure, done.", False),
            ("Really?  ", True),
            ("No.", False),
            ("Alright\n", False),
        ]
        for text, want in cases:
            with self.subTest(text):
                self.assertEqual(ends_with_question(text), want)

    def test_elevation(self):
        cases = [
            ("flag true", {"elevated": True}, None, True),
            ("flag false", {"elevated": False}, 100.0, False),
            ("deadline ahead", {"elevatedUntil": 100.0}, 50.0, True),
            ("deadline passed", {"elevatedUntil": 100.0}, 150.0, False),
            ("until alias", {"until": 100.0}, 99.0, True),
            ("no session", None, None, False),
            ("empty session", {}, None, False),
            ("odd value", {"elevatedUntil": "soon"}, None, False),
        ]
        for name, session, now, want in cases:
            with self.subTest(name):
                self.assertEqual(session_elevated(session, now=now), want)

    def test_presence(self):
        cases = [
            ({}, build_presence()),
            ({"area": "Kitchen"}, build_presence(area="Kitchen")),
            ({"area": "Kitchen", "people": ["A"], "occupied": True},
             build_presence(area="Kitchen", people=["A"], occupied=True)),
            ({"people": [], "occupied": False},
             build_presence(people=[], occupied=False)),
        ]
        for want, got in cases:
            with self.subTest(str(want)):
                self.assertEqual(got, want)


class FixedSentencesTest(unittest.TestCase):
    def test_sentences_exist(self):
        # The canned lines must stay short enough to speak and must never be
        # empty; the component always falls back to one of these three.
        for text in (OFFLINE_TEXT, ERROR_TEXT, TIMEOUT_TEXT):
            self.assertTrue(text)
            self.assertLess(len(text), 200)
        self.assertNotEqual(OFFLINE_TEXT, ERROR_TEXT)


if __name__ == "__main__":
    unittest.main()
