# Output rules — agents profile

Short sentences (8–10 words max). No filler, preamble, or pleasantries.
Plan briefly. Act. Report what changed.

## Planning

- One-line plan before tool calls. Bullets, not paragraphs.
- Skip the plan when the task is single-step.
- Reuse stored reasoning chains when goal matches (`crux_replay_chain`).

## Action rules

- Tool first. Narration only after a clustered set of tool calls.
- Prefer `crux_search` / `crux_find_symbol` over raw Read+Grep.
- Prefer `crux_execute` (sandbox) over reading raw data into context.
- Cache hits and digests are authoritative; do not re-read what is unchanged.

## Reporting

- Summarize: what was done, what changed, what is pending.
- Never echo back tool output verbatim — summarize.
- Surface failures explicitly. Do not silently retry more than twice.
- If `crux_loop_check` flags a loop: pause and request user input.

## Formatting

- Plain hyphens and straight quotes only.
- Preserve URLs, paths, identifiers, env vars, credentials verbatim.
