"""Table-driven tests for agent_core: scripted fake transport, fake tools.

Run with:  python -B -m unittest test_agent_core   (bytecode writing off keeps
the tree clean). The golden prompt file `agent_prompt.golden.txt` is part of
these fixtures; regenerate deliberately with write_golden().
"""

import asyncio
import json
import os
import unittest

import agent_core
from agent_core import Config, TransportError


GOLDEN_PATH = os.path.join(os.path.dirname(__file__), "agent_prompt.golden.txt")

FIX_CTX = {
    "now": "2026-11-26 18:00",
    "room": "Kitchen",
    "areas": {
        "Kitchen": {"lights": ["Kitchen lights"],
                    "climates": ["Kitchen Radiator"]},
        "Living Room": {"lights": ["Sofa lamp"],
                        "climates": ["Living Room Radiator"]},
    },
}


def fixture_schemas():
    """A trimmed stand-in for homecontrol.schemas(); enough that known tool
    names exist without importing homecontrol (homecontrol owns its own schema test)."""
    def fn(name, props):
        return {"type": "function",
                "function": {"name": name,
                             "description": name + " house tool",
                             "parameters": {"type": "object",
                                            "properties": props}}}
    return [fn("light", {"target": {"type": "string"},
                         "action": {"type": "string"}}),
            fn("climate", {"target": {"type": "string"},
                           "action": {"type": "string"}}),
            fn("timer", {"action": {"type": "string"}}),
            fn("home_state", {"target": {"type": "string"}})]


def build_system_text():
    cfg = Config(model_url="http://x/v1/chat/completions", model_name="m")
    return agent_core.SYSTEM_PROMPT + "\n\n" + agent_core.build_context(
        FIX_CTX, cfg, clock=lambda: 0.0)


def write_golden():
    with open(GOLDEN_PATH, "w", encoding="utf-8", newline="\n") as fh:
        fh.write(build_system_text())


class FakeTransport:
    """Scripted chat-completions endpoint. Script entries are dicts:
       {"msg": {...}}            -> 200 with that assistant message
       {"status": n, "body": s}  -> that status instead
       "offline" | "timeout"     -> raise like a dead/slow endpoint
    Every request is recorded: (url, headers, parsed_body)."""

    def __init__(self, script):
        self.script = list(script)
        self.seen = []

    async def request(self, method, url, headers=None, data=None, timeout=None):
        body = json.loads(data.decode("utf-8"))
        self.seen.append({"url": url, "headers": dict(headers or {}), "body": body})
        if not self.script:
            raise AssertionError("model called more times than scripted")
        item = self.script.pop(0)
        if item == "offline":
            raise TransportError("connection refused")
        if item == "timeout":
            raise TimeoutError()
        status = item.get("status", 200)
        payload = json.dumps({"choices": [{"message": item["msg"],
                                           "finish_reason": item.get("finish")}]}).encode()
        return status, payload


class FakeTools:
    def __init__(self, outcomes=None, raises=False):
        self.schemas = fixture_schemas()
        self.outcomes = list(outcomes or [])
        self.raises = raises
        self.calls = []

    async def execute(self, name, args):
        self.calls.append((name, dict(args)))
        if self.raises:
            raise RuntimeError("boom")
        return self.outcomes.pop(0) if self.outcomes else {"ok": True}


def msg_tool(calls, content=None, finish=None):
    return {"msg": {"content": content,
                    "tool_calls": [{"id": c["id"], "type": "function",
                                    "function": {"name": c["name"],
                                                 "arguments": c.get("args", "")}}
                                   for c in calls]},
            "finish": finish}


def msg_text(content, finish=None):
    return {"msg": {"content": content}, "finish": finish}


def run_case(script, cfg_extra=None, tools_kwargs=None, text="turn on the kitchen lights",
             tags=None, ctx=FIX_CTX):
    kwargs = {"model_url": "http://main/v1/chat/completions",
              "model_name": "qwen3.5:4b", "extra_body": {"think": True}}
    kwargs.update(cfg_extra or {})
    cfg = Config(**kwargs)
    transport = FakeTransport(script)
    tools = FakeTools(**(tools_kwargs or {}))
    result = asyncio.run(agent_core.run_turn(cfg, transport, tools, ctx, text,
                                             tags=tags, clock=lambda: 0.0))
    return result, transport, tools


class AgentCoreTest(unittest.TestCase):
    def test_tool_call_then_final_sentence(self):
        r, tr, tools = run_case(
            [msg_tool([{"id": "1", "name": "light",
                        "args": '{"target": "Kitchen", "action": "on"}'}]),
             msg_text("The kitchen lights are on.")],
            tools_kwargs={"outcomes": [{"ok": True, "text": "turned on 2 lights"}]})
        self.assertEqual(r.kind, agent_core.KIND_ANSWER)
        self.assertEqual(r.text, "The kitchen lights are on.")
        self.assertEqual(tools.calls, [("light", {"target": "Kitchen",
                                                  "action": "on"})])
        self.assertEqual(len(tr.seen), 2)
        self.assertEqual(r.tool_calls[0]["name"], "light")
        self.assertTrue(r.tool_calls[0]["ok"])

    def test_two_chained_tool_calls(self):
        r, tr, tools = run_case(
            [msg_tool([{"id": "1", "name": "light",
                        "args": {"target": "Kitchen", "action": "on"}}]),
             msg_tool([{"id": "2", "name": "climate",
                        "args": {"target": "Kitchen Radiator", "action": "set",
                                 "temperature": 21}}]),
             msg_text("Lights on, heating set to twenty-one.")],
            tools_kwargs={"outcomes": [{"ok": True}, {"ok": True}]})
        self.assertEqual(r.kind, agent_core.KIND_ANSWER)
        self.assertEqual([c[0] for c in tools.calls], ["light", "climate"])

    def test_malformed_tool_json_parsed_leniently(self):
        # args as a JSON-in-string with stray prose around the object
        r, _, tools = run_case(
            [msg_tool([{"id": "1", "name": "light",
                        "args": 'Sure! {"target": "Kitchen", "action": "toggle"} done'}]),
             msg_text("Toggled.")],
            tools_kwargs={"outcomes": [{"ok": True}]})
        self.assertEqual(tools.calls[0][1], {"target": "Kitchen", "action": "toggle"})
        self.assertEqual(r.kind, agent_core.KIND_ANSWER)

    def test_unknown_tool_told_once_then_model_recovers(self):
        r, tr, tools = run_case(
            [msg_tool([{"id": "1", "name": "turn_on_kitchen", "args": {}}]),
             msg_tool([{"id": "2", "name": "light",
                        "args": {"target": "Kitchen", "action": "on"}}]),
             msg_text("Kitchen lights on.")],
            tools_kwargs={"outcomes": [{"ok": True}]})
        self.assertEqual(tools.calls, [("light", {"target": "Kitchen", "action": "on"})])
        # the tool message that went back to the model says it is unknown
        fed = [m for m in tr.seen[1]["body"]["messages"] if m["role"] == "tool"]
        self.assertIn("unknown tool", json.dumps(fed))
        self.assertEqual(r.kind, agent_core.KIND_ANSWER)

    def test_bad_arguments_twice_escalates(self):
        r, tr, _ = run_case(
            [msg_tool([{"id": "1", "name": "light", "args": "{{{ not json"}]),
             msg_tool([{"id": "2", "name": "light", "args": "no object here"}]),
             msg_text("Let me think about that differently.")],
            cfg_extra={"escalate_url": "http://flash/v1/chat/completions"})
        self.assertEqual(r.kind, agent_core.KIND_ANSWER)
        self.assertEqual(r.model_used, "escalate")
        self.assertEqual(tr.seen[2]["url"], "http://flash/v1/chat/completions")

    def test_bad_arguments_twice_without_escalation_fixed_sentence(self):
        r, tr, _ = run_case(
            [msg_tool([{"id": "1", "name": "light", "args": "{{{"}]),
             msg_tool([{"id": "2", "name": "light", "args": "}}}"}])])
        self.assertEqual(r.kind, agent_core.KIND_ERROR)
        self.assertEqual(r.text, agent_core.ERROR_TEXT)

    def test_thinking_block_stripped(self):
        r, _, _ = run_case([msg_text(
            "<think>The user wants the kitchen lights. I should call light."
            "</think>The kitchen lights are on.")])
        self.assertEqual(r.text, "The kitchen lights are on.")

    def test_unclosed_thinking_leaves_nothing(self):
        r, _, _ = run_case([msg_text("<think>endless reasoning about bulbs")],
                           cfg_extra={"escalate_url": None})
        self.assertEqual(r.kind, agent_core.KIND_ERROR)
        self.assertEqual(r.text, agent_core.EMPTY_TEXT)

    def test_markdown_lists_and_urls_cleaned(self):
        r, _, _ = run_case([msg_text(
            "**On.**\n- first option\n- second option\n"
            "Details at https://example.com/x and [a link](http://y.z)")])
        self.assertNotIn("*", r.text)
        self.assertNotIn("http", r.text)
        self.assertNotIn("\n", r.text)
        self.assertIn("first option", r.text)

    def test_overlong_reply_cut_near_sentence_boundary(self):
        long = " ".join("Sentence number %d explains the house settings." % i
                        for i in range(30))
        r, _, _ = run_case([msg_text(long)])
        self.assertLessEqual(len(r.text), 325)
        self.assertTrue(r.text.startswith("Sentence number 0"))

    def test_timeout_speaks_fixed_sentence(self):
        r, _, _ = run_case(["timeout"])
        self.assertEqual(r.kind, agent_core.KIND_TIMEOUT)
        self.assertEqual(r.text, agent_core.TIMEOUT_TEXT)

    def test_offline_endpoint_then_escalation_answers(self):
        r, tr, _ = run_case(
            ["offline", msg_text("It is Thursday.")],
            cfg_extra={"escalate_url": "http://flash/v1/chat/completions",
                       "escalate_model": "gemini-flash"})
        self.assertEqual(r.kind, agent_core.KIND_ANSWER)
        self.assertEqual(r.model_used, "escalate")
        self.assertEqual([s["url"] for s in tr.seen],
                         ["http://main/v1/chat/completions",
                          "http://flash/v1/chat/completions"])

    def test_both_endpoints_down_offline_sentence(self):
        r, _, _ = run_case(["offline", "offline"],
                           cfg_extra={"escalate_url": "http://flash/x"})
        self.assertEqual(r.kind, agent_core.KIND_OFFLINE)
        self.assertEqual(r.text, agent_core.OFFLINE_TEXT)

    def test_capped_output_escalates(self):
        r, tr, _ = run_case(
            [msg_text("<think>" + "probing ideas " * 40, finish="length"),
             msg_text("Set it up for you.")],
            cfg_extra={"escalate_url": "http://flash/v1/chat/completions"})
        self.assertEqual(r.kind, agent_core.KIND_ANSWER)
        self.assertEqual(r.model_used, "escalate")

    def test_ask_helm_returns_escalate_kind(self):
        r, _, _ = run_case([msg_tool([{"id": "1", "name": "ask_helm",
                                       "args": {"text": "what is on my calendar"}}])])
        self.assertEqual(r.kind, agent_core.KIND_ESCALATE)
        self.assertEqual(r.escalate_text, "what is on my calendar")
        self.assertEqual(r.model_used, "qwen3.5:4b")

    def test_history_window_respects_ttl_and_turns(self):
        ctx = dict(FIX_CTX)
        ctx["history"] = [
            {"user": "older question", "spoken": "older answer", "ts": -500},
            {"user": "dim it", "spoken": "Dimmed.", "ts": 0},
        ]
        r, tr, _ = run_case([msg_text("Done.")], ctx=ctx)
        msgs = tr.seen[0]["body"]["messages"]
        roles = [m["content"] for m in msgs if m["role"] == "user"]
        self.assertIn("dim it", roles)
        self.assertNotIn("older question", roles)

    def test_tool_budget_exhausted_fixed_sentence(self):
        r, tr, tools = run_case(
            [msg_tool([{"id": "1", "name": "light",
                        "args": {"target": "Kitchen", "action": "on"}}]),
             msg_tool([{"id": "2", "name": "light",
                        "args": {"target": "Kitchen", "action": "off"}}])],
            cfg_extra={"max_tool_rounds": 1},
            tools_kwargs={"outcomes": [{"ok": True}]})
        self.assertEqual(r.kind, agent_core.KIND_ERROR)
        self.assertEqual(len(tools.calls), 1)
        self.assertEqual(len(tr.seen[0]["body"]["messages"]), 2)

    def test_headers_and_extra_body_pass_through(self):
        r, tr, _ = run_case(
            [msg_text("Okay.")],
            cfg_extra={"api_key": "secret-token",
                       "extra_headers": {"X-Strata-Priority": "now"},
                       "extra_body": {"think": True, "format": "markdown"}})
        seen = tr.seen[0]
        self.assertEqual(seen["headers"]["Authorization"], "Bearer secret-token")
        self.assertEqual(seen["headers"]["X-Strata-Priority"], "now")
        self.assertTrue(seen["body"]["think"])
        self.assertEqual(seen["body"]["format"], "markdown")
        self.assertEqual(seen["body"]["model"], "qwen3.5:4b")

    def test_timings_present(self):
        r, _, _ = run_case(
            [msg_tool([{"id": "1", "name": "light",
                        "args": {"target": "Kitchen", "action": "on"}}]),
             msg_text("Done.")],
            tools_kwargs={"outcomes": [{"ok": True}]})
        for key in ("prompt_ms", "model_ms", "tool_ms", "total_ms"):
            self.assertIn(key, r.timings)
        self.assertEqual(len(r.timings["model_ms"]), 2)
        self.assertEqual(len(r.timings["tool_ms"]), 1)

    def test_tags_and_prompt_hash_carried_through(self):
        r1, _, _ = run_case([msg_text("Hi.")], tags={"arm": "A", "run": 3})
        r2, _, _ = run_case([msg_text("Hi.")], tags={"arm": "A", "run": 3})
        self.assertEqual(r1.tags, {"arm": "A", "run": 3})
        self.assertEqual(len(r1.prompt_hash), 16)
        self.assertEqual(r1.prompt_hash, r2.prompt_hash)


class PromptSizeTest(unittest.TestCase):
    def test_system_prompt_under_250_tokens(self):
        self.assertLess(agent_core.estimate_tokens(agent_core.SYSTEM_PROMPT), 250)

    def test_assembled_prompt_matches_golden_and_fits_budget(self):
        system = build_system_text()
        with open(GOLDEN_PATH, encoding="utf-8") as fh:
            golden = fh.read()
        self.assertEqual(system, golden,
                         "assembled prompt changed - regenerate deliberately")
        schemas = fixture_schemas() + [agent_core.ASK_HELM_SCHEMA]
        tool_tokens = agent_core.estimate_tokens(
            json.dumps(schemas, sort_keys=True))
        total = (agent_core.estimate_tokens(agent_core.SYSTEM_PROMPT)
                 + agent_core.estimate_tokens(system.split("\n\n", 1)[1])
                 + tool_tokens + agent_core.estimate_tokens(
                     "turn on the kitchen lights"))
        self.assertLess(total, 1200)

    def test_golden_file_exists(self):
        self.assertTrue(os.path.exists(GOLDEN_PATH))


class ParsingTest(unittest.TestCase):
    def test_parse_tool_args_table(self):
        cases = [
            (None, {}),
            ({"a": 1}, {"a": 1}),
            ('{"a": 1}', {"a": 1}),
            ('"{\\"a\\": 1}"', {"a": 1}),
            ('Sure: {"a": 1} thanks', {"a": 1}),
        ]
        for raw, want in cases:
            self.assertEqual(agent_core.parse_tool_args(raw), want)

    def test_parse_tool_args_rejects_garbage(self):
        for raw in ["no braces at all", True, "['list']", "[1, 2]"]:
            with self.assertRaises(ValueError):
                agent_core.parse_tool_args(raw)

    def test_salvage_tool_call_block_from_content(self):
        kept, calls = agent_core.salvage_tool_calls(
            'Turning it on. <tool_call>{"name": "light", "arguments": '
            '{"target": "Kitchen"}}</tool_call> thanks')
        self.assertEqual(len(calls), 1)
        self.assertEqual(calls[0]["name"], "light")
        self.assertIn("Turning it on.", kept)
        self.assertNotIn("tool_call", kept)

    def test_ends_with_question(self):
        self.assertTrue(agent_core.ends_with_question("Which room? "))
        self.assertFalse(agent_core.ends_with_question("Done."))


if __name__ == "__main__":
    if not os.path.exists(GOLDEN_PATH):
        write_golden()
    unittest.main()
