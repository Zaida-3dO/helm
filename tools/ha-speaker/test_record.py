"""hubapi additions: /speaker/record (best effort, 404 means "no such
route") and the escalation context on /speaker/turn. Same scripted
transport style as test_hubapi."""

import json
import unittest

import hubapi
from hubapi import HubApi, TransportError


class FakeTransport:
    def __init__(self, script=()):
        self.script = list(script)
        self.calls = []

    async def request(self, method, url, *, headers=None, data=None, timeout=None):
        self.calls.append({"method": method, "url": url,
                           "headers": headers or {}, "data": data})
        if not self.script:
            raise AssertionError("transport called more often than scripted")
        action = self.script.pop(0)
        if action is None:
            raise TransportError("connection refused")
        status, payload = action
        body = payload if isinstance(payload, bytes) else json.dumps(payload).encode()
        return status, body


def make_api(script=()):
    transport = FakeTransport(script)
    return HubApi("http://hub.example:8480", "tok-1", transport), transport


class FlagResetMixin(unittest.TestCase):
    def setUp(self):
        hubapi._RECORD_UNSUPPORTED = False

    def tearDown(self):
        hubapi._RECORD_UNSUPPORTED = False


def run(coro):
    import asyncio
    return asyncio.run(coro)


class RecordTest(FlagResetMixin):
    def test_posts_the_summary(self):
        api, t = make_api([(200, {"ok": True})])
        out = run(api.record(turn_id="t9", device="dev-1", room="Kitchen",
                              text="lights on", answer="Done.", kind="answer",
                              tool_calls=[{"name": "light", "ok": True}],
                              timings={"total_ms": 900}, model="qwen",
                              tags={"arm": "A"}))
        self.assertEqual(out["kind"], "ok")
        call = t.calls[0]
        self.assertTrue(call["url"].endswith("/speaker/record"))
        self.assertEqual(call["headers"]["Authorization"], "Bearer tok-1")
        body = json.loads(call["data"].decode())
        self.assertEqual(body["turn_id"], "t9")
        self.assertEqual(body["answer"], "Done.")
        self.assertEqual(body["tool_calls"][0]["name"], "light")

    def test_404_stops_trying_for_the_process(self):
        api, t = make_api([(404, b""), (404, b"")])
        first = run(api.record(turn_id="a"))
        self.assertEqual(first["kind"], "unsupported")
        second = run(api.record(turn_id="b"))
        self.assertEqual(second["kind"], "unsupported")
        self.assertEqual(len(t.calls), 1)  # the second record never left

    def test_hub_down_is_quiet(self):
        api, _t = make_api([None])
        self.assertEqual(run(api.record(turn_id="c"))["kind"], "offline")

    def test_body_capped_at_32k(self):
        big = [{"name": "light", "ok": True,
                "args": {"junk": "x" * 200}} for _ in range(400)]
        api, t = make_api([(200, {"ok": True})])
        run(api.record(turn_id="d", tool_calls=big,
                       timings={str(i): i for i in range(500)},
                       answer="y" * 40000))
        sent = t.calls[0]["data"]
        self.assertLessEqual(len(sent), hubapi.MAX_RECORD_BYTES)
        body = json.loads(sent.decode())
        self.assertEqual(body["turn_id"], "d")

    def test_no_turn_id_no_call(self):
        api, t = make_api([])
        run(api.record(turn_id=None))
        self.assertEqual(t.calls, [])


class EscalationContextTest(FlagResetMixin):
    def base_kwargs(self):
        return dict(text="what did I promise?", turn_id="x1")

    def test_context_and_origin_present_when_escalating(self):
        api, t = make_api([(200, {"state": "answer", "text": "You promised"})])
        run(api.run_turn(**self.base_kwargs(),
                         context={"recent": [{"ask": "a", "said": "b"}] * 50},
                         escalation=True))
        body = json.loads(t.calls[0]["data"].decode())
        self.assertEqual(body["origin"], "escalation")
        raw = json.dumps(body["context"]).encode()
        self.assertLessEqual(len(raw), hubapi.MAX_CONTEXT_BYTES)
        self.assertIn("recent", body["context"])

    def test_plain_turn_has_neither_key(self):
        api, t = make_api([(200, {"state": "answer", "text": "Done"})])
        run(api.run_turn(**self.base_kwargs()))
        body = json.loads(t.calls[0]["data"].decode())
        self.assertNotIn("origin", body)
        self.assertNotIn("context", body)


if __name__ == "__main__":
    unittest.main()
