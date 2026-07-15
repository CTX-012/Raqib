# JOURNAL — append-only session log

*Append a dated entry after EVERY landing. This is how you (the agent) remember what happened across sessions — you have no memory between them, so WRITE IT DOWN. Newest at the bottom. Keep entries short: what shipped, tests delta, commit, anything pending.*

*Format:*
```
## YYYY-MM-DD — <short title>
- Commit: <hash> — <one-line what>
- Tests: <before> → <after> (delta reason)
- Gate: <browser gate before> → <after> if web-touched
- Dormant/pending: <what's built-but-not-live, or what needs human>
- Notes: <anything the next session needs to know>
```

---

## 2026-07-15 — autonomy structure installed (baseline)
- Baseline: v1.3.2-36-g729bdf7, 1181 tests, gate 221/0, contract v0.3.21.
- Three arcs complete (auto-kill, history, display modes), both cleanup chunks done.
- This JOURNAL + BOARD + CLAUDE.md + PENDING.md installed for autonomous operation.
- Next candidate work: TUI essentials-only (needs design first — HARD STOP #3), or GPU temp/power tile (low-risk).
- Two human decisions pending: versioning (v2.0.0?), observer→supervisor.
