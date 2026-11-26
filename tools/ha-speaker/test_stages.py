"""Timing records, rotation and the replay engine - no Home Assistant
needed. Same synthetic-package loader as test_ha_glue."""

import asyncio
import importlib
import json
import os
import sys
import tempfile
import types
import unittest

_HERE = os.path.dirname(os.path.abspath(__file__))
_PKG = types.ModuleType("hs")
_PKG.__path__ = [_HERE]
sys.modules.setdefault("hs", _PKG)

agent_core = importlib.import_module("hs.agent_core")
replay = importlib.import_module("hs.replay")
stages = importlib.import_module("hs.stages")
timings = importlib.import_module("hs.timings")

TurnResult = agent_core.TurnResult


class Sink:
    def __init__(self):
        self.records = []

    def append(self, record):
        self.records.append(record)


def result(**over):
    base = dict(
        text="The kitchen light is on.", kind="answer",
        tool_calls=[{"name": "light", "args": {"action": "on"}, "ok": True,
                     "ms": 5}],
        timings={"prompt_ms": 12, "model_ms": [300, 400], "tool_ms": [5],
                 "total_ms": 800},
        model_used="qwen3.5:4b", prompt_hash="ab12cd34ef56ab12", tags={"arm": "A"})
    base.update(over)
    return TurnResult(**base)


class StageLogTest(unittest.TestCase):
    def test_record_fields_from_seen_stages(self):
        sink = Sink()
        log = stages.StageLog(sink)
        stt = {"listen_start_ts": 100.0, "collected_ts": 102.5,
               "done_ts": 103.0, "listened_s": 2.5, "stt_ms": 0.5}
        rec = log.finish_turn(turn_id="t1", device="dev-1", profile="path_a",
                              result=result(), kind="answer", tags={"arm": "A"},
                              stt=stt, text_in="lights on",
                              text_out="The kitchen light is on.", now=200.0)
        self.assertEqual(rec["stage_ms"]["speech"], 2500)
        self.assertEqual(rec["stage_ms"]["stt"], 500)
        self.assertEqual(rec["stage_ms"]["model"], 700)
        self.assertEqual(rec["stage_ms"]["tool"], [5])
        self.assertEqual(rec["stage_ms"]["answer_ready"], 800)
        self.assertEqual(rec["t"]["wake"], 100000)
        self.assertEqual(rec["t"]["speech_end"], 102500)
        self.assertEqual(rec["t"]["stt_done"], 103000)
        self.assertEqual(rec["t"]["answer_ready"], 200000)
        self.assertEqual(rec["profile"], "path_a")
        self.assertEqual(rec["tool_calls"][0]["name"], "light")
        self.assertIsNone(rec["stage_ms"]["wake_to_listen"])
        self.assertIsNone(rec["t"]["tts_end"])

    def test_unseen_stages_are_null_not_zero(self):
        sink = Sink()
        log = stages.StageLog(sink)
        rec = log.finish_turn(turn_id="t2", result=result(), now=50.0)
        for key in ("wake", "speech_end", "stt_done", "tts_start", "tts_end"):
            self.assertIsNone(rec["t"][key], key)
        self.assertIsNone(rec["stage_ms"]["speech"])
        self.assertIsNone(rec["stage_ms"]["stt"])
        # model numbers still real, from the agent's own timings
        self.assertEqual(rec["stage_ms"]["model"], 700)

    def test_no_secret_in_any_record(self):
        sink = Sink()
        log = stages.StageLog(sink)
        rec = log.finish_turn(turn_id="t3", result=result(), now=1.0)
        self.assertNotIn("sk-live", json.dumps(rec))


class TimingWriterTest(unittest.TestCase):
    def test_append_and_rotation(self):
        tmp = tempfile.mkdtemp()
        path = os.path.join(tmp, "helm_speaker", "timings.jsonl")
        writer = timings.TimingWriter(path, max_bytes=25)
        writer.append({"n": 1})
        writer.append({"n": 2})
        with open(path, encoding="utf-8") as fh:
            self.assertEqual(len(fh.read().splitlines()), 2)
        writer.append({"n": 3})  # passes the tiny cap -> rotates
        rotated = path + ".1"
        self.assertTrue(os.path.exists(rotated))
        with open(rotated, encoding="utf-8") as fh:
            self.assertEqual(len(fh.read().splitlines()), 3)
        writer.append({"n": 4})
        with open(path, encoding="utf-8") as fh:
            self.assertEqual(fh.read().strip(), '{"n": 4}')


class _Entry:
    def __init__(self, runtime_data, options=None):
        self.runtime_data = runtime_data
        self.options = options or {}


class SttJoinTest(unittest.TestCase):
    def test_take_stt_fresh_then_gone(self):
        entry = _Entry({"stt_last": {"done_ts": 10.0, "stt_ms": 0.4}})
        got = stages.take_stt(entry, done_not_before=5.0)
        self.assertEqual(got["stt_ms"], 0.4)
        self.assertIsNone(stages.take_stt(entry, done_not_before=5.0))

    def test_take_stt_rejects_old_capture(self):
        entry = _Entry({"stt_last": {"done_ts": 1.0}})
        self.assertIsNone(stages.take_stt(entry, done_not_before=5.0))

    def test_note_turn_without_log_is_harmless(self):
        entry = _Entry({})
        stages.note_turn(None, entry, device_id=None, turn_id="abc12345",
                         text_in="x", text_out="y", kind="answer",
                         result=result())  # no exception, nothing written


class ScriptedTransport:
    """Answers like an OpenAI chat endpoint: scripted bodies in order."""

    def __init__(self, bodies):
        self.bodies = list(bodies)
        self.seen_data = []

    async def request(self, method, url, *, headers=None, data=None,
                      timeout=None):
        self.seen_data.append(json.loads(data.decode("utf-8")))
        if not self.bodies:
            raise agent_core.TransportError("script empty")
        return 200, json.dumps(self.bodies.pop(0)).encode("utf-8")


def chat(message, finish="stop"):
    return {"choices": [{"message": message, "finish_reason": finish}]}


class ReplayGoldenTest(unittest.TestCase):
    def test_run_set_dry_tool_chain_and_expect(self):
        transport = ScriptedTransport([
            chat({"role": "assistant", "content": None, "tool_calls": [
                {"id": "c1", "type": "function", "function": {
                    "name": "light",
                    "arguments": json.dumps(
                        {"target": "Kitchen lights", "action": "on"})}}]}),
            chat({"role": "assistant",
                  "content": "The kitchen light is on."}),
        ])
        house = {"Kitchen": {
            "lights": [["Kitchen lights", "light.kitchen"]],
            "climates": []}}
        control = replay.HomeControl(replay.CliBackend(house), dry=True)
        cfg = agent_core.Config(model_url="http://model.test/v1/chat",
                                model_name="qwen3.5:4b",
                                api_key="sk-live-secret")
        sink = Sink()
        rows = replay.load_set([
            '{"id": "lights-on", "text": "turn on the kitchen lights",'
            ' "expect": {"tool": "light", "args": {"action": "on"}}}'])
        written = asyncio.run(replay.run_set(
            rows, config=cfg, control=control, emit=sink,
            transport=transport, tags={"arm": "A"}))
        self.assertEqual(len(written), 1)
        rec = written[0]
        self.assertEqual(rec["kind"], "answer")
        self.assertEqual(rec["expect_match"], True)
        self.assertEqual(rec["tool_calls"][0]["name"], "light")
        self.assertTrue(rec["tool_calls"][0]["ok"])
        # the tool answer came back into the second model call
        roles = [m["role"] for m in transport.seen_data[1]["messages"]]
        self.assertIn("tool", roles)
        blob = json.dumps(sink.records)
        self.assertNotIn("sk-live-secret", blob)
        summary = replay.summarize(written)
        self.assertEqual(summary[0]["rows"], 1)
        self.assertEqual(summary[0]["match"], "1/1")


if __name__ == "__main__":
    unittest.main()
