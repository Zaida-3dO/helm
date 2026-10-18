# Plan-usage meter: what is known and what is not

Recorded, not guessed. Observed on a Windows dev machine with Claude Code 2.1.289 (2026-10-18)
unless noted.

## The control request

- `helpers/usage` starts `claude` in stream-json print mode with an empty strict MCP config and no
  setting sources, then sends three control requests and **no user message**: `initialize`,
  `get_binary_version` and `get_usage` (`skip_behaviors: true`). No model call is made.
- The `get_usage` answer carries `subscription_type`, `rate_limits_available` and `rate_limits`.
  Inside `rate_limits`, the fields Helm reads are `five_hour` and `seven_day` (each
  `utilization`, a percent, and `resets_at`, an ISO time) and `model_scoped` (a list of
  `display_name`, `utilization`, `resets_at`). `seven_day_opus` and `seven_day_sonnet` are read when
  present. The answer also carries `limits`, `extra_usage` and `seven_day_breakdown`; Helm ignores
  them.
- The windows seen with a Max plan: the 5-hour window, the weekly window, and one per-model weekly
  window (named by the binary's display name).
- The request is marked experimental upstream. A binary that answers with an error, exits or says
  nothing is the "unsupported" readiness state, not a failure. A sign-in with no plan limits
  (an API key or another provider) is the "no limits" state.
- `get_binary_version` is asked first so the readiness text can name the version. It is optional:
  an answer of any other kind leaves the version unknown.

## `rate_limit_event`

- A chat's worker turns each `rate_limit_event` into a `plan_usage` line. Its `rateLimitType` is the
  window (`five_hour`, `seven_day`, `seven_day_opus`, `seven_day_sonnet`; `overage` is ignored),
  `resetsAt` is epoch **seconds**, and `utilization` is read as a fraction when it is 1 or less, else
  as a percent.
- **Unknown:** `utilization` appears only near a threshold, so most events carry a reset time and no
  number. The host never lets such a line wipe a number it holds for the same window.
- **Unknown:** whether a 1% reading arrives as `0.01` or `1`. The worker treats 1 or less as a
  fraction, so a true 1% would show as 100% until the next control read (at most 10 minutes later).
  The live check compares the meter with a second, direct run of the usage
  helper to catch it.

## Cadence

On launch, every 10 minutes while the window is visible, and on Refresh; never more often than once
per 60 s, enforced by the host. A reading is stale after 15 minutes.

## Release builds

The helper is found beside the staged worker tree (`<resources>/helpers/usage`). `tauri.conf.json`
bundles it as a resource, with the titles and autocomplete helpers; a release build that lacks it
reports "Helm could not find its usage helper".
