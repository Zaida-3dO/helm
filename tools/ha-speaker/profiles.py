"""Model-profile plumbing: entry data and options in, agent_core.Config out.

A config entry carries one live setup (the fields the user typed) plus an
optional ``options`` block holding two named profiles - ``path_a`` and
``path_b``, one active at a time. That is how the benchmark swaps arms
without a restart: rewrite options, rebuild one Config, done. Secrets (the
model key) never leave this module towards a log line: use redact().

Pure standard library so it can be tested without Home Assistant loaded;
only ``agent_core`` is imported, and that is pure too.
"""

import json

from .agent_core import Config
from .const import (
    CONF_EXTRA_BODY,
    CONF_EXTRA_HEADERS,
    CONF_MODEL_KEY,
    CONF_MODEL_NAME,
    CONF_MODEL_URL,
    CONF_PROFILE,
    CONF_THINKING,
    CONF_TIMEOUT_S,
    CONF_TOKEN,
    CONF_URL,
    MAX_JSON_FIELD,
    PROFILES,
)


def parse_json_field(text):
    """A free-text JSON box in the UI -> (dict_or_None, error_key_or_None)."""
    if text is None:
        return {}, None
    text = str(text).strip()
    if not text:
        return {}, None
    if len(text) > MAX_JSON_FIELD:
        return None, "json_too_long"
    try:
        value = json.loads(text)
    except ValueError:
        return None, "bad_json"
    if not isinstance(value, dict):
        return None, "bad_json"
    return value, None


def profile_from_form(form):
    """Turn one form submission into a stored profile dict."""
    headers, herr = parse_json_field(form.get(CONF_EXTRA_HEADERS))
    body, berr = parse_json_field(form.get(CONF_EXTRA_BODY))
    if herr or berr:
        raise ValueError(herr or berr)
    return {
        "model_url": (form.get(CONF_MODEL_URL) or "").strip().rstrip("/"),
        "model_name": (form.get(CONF_MODEL_NAME) or "").strip(),
        "api_key": form.get(CONF_MODEL_KEY) or None,
        "thinking": bool(form.get(CONF_THINKING, True)),
        "extra_headers": headers or {},
        "extra_body": body or {},
        "timeout_s": float(form.get(CONF_TIMEOUT_S) or 30),
    }


def profiles_from_entry(data, options):
    """Both named profiles for an entry. With no options yet (an entry made
    before profiles existed, or a fresh single setup) both start from the
    live fields, so switching is always safe."""
    stored = (options or {}).get("profiles") or {}
    base = {
        "model_url": data.get(CONF_MODEL_URL) or "",
        "model_name": data.get(CONF_MODEL_NAME) or "",
        "api_key": data.get(CONF_MODEL_KEY),
        "thinking": bool(data.get(CONF_THINKING, True)),
        "extra_headers": data.get("headers") or {},
        "extra_body": data.get("body") or {},
        "timeout_s": float(data.get(CONF_TIMEOUT_S) or 30),
    }
    out = {}
    for name in PROFILES:
        merged = dict(base)
        merged.update(stored.get(name) or {})
        out[name] = merged
    return out


def active_profile(options):
    name = (options or {}).get(CONF_PROFILE)
    return name if name in PROFILES else PROFILES[0]


def build_model_config(data, options):
    """The Config for the active profile. The hub fields are deliberately
    not here: escalation to a second model endpoint is only what a profile's
    own extra settings give it; the household hub runs through hubapi."""
    profile = profiles_from_entry(data, options)[active_profile(options)]
    return Config(
        model_url=profile["model_url"],
        model_name=profile["model_name"],
        api_key=profile["api_key"] or None,
        extra_headers=dict(profile["extra_headers"]),
        extra_body=dict(profile["extra_body"]),
        thinking=profile["thinking"],
        timeout_s=profile["timeout_s"],
    )


def redact(profile):
    """A logging-safe view of a profile: the key becomes ***."""
    out = dict(profile)
    if out.get("api_key"):
        out["api_key"] = "***"
    return out
