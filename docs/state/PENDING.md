# PENDING — things waiting on the human

*When you (the agent) hit a HARD STOP, write it here LOUDLY and stop. The human reads this at milestone check-ins. Clear an item when it's resolved (move the resolution to JOURNAL.md).*

*Format:*
```
## [STOP #N] <title> — <date>
**What I was doing:** ...
**Why I stopped:** (which HARD STOP rule)
**What I need from you:** (a decision / a CAR / a governor review / driver reload / etc.)
**My recommendation (if any):** ...
**What's safe to do meanwhile:** (other work I can proceed with, or "nothing — blocked")
```

---

## (no pending items)

The agent has not hit a HARD STOP yet. When it does, entries appear above this line.

---

### Reference — the HARD STOP rules (from CLAUDE.md)
1. Governor / kill / actuation path touched — surface, never auto-proceed
2. A contract change (`../ux_contract`, new wire type, new endpoint) is needed — write a CAR, stop
3. An unratified design/UX decision (materially different approaches, no doc settles it) — propose options, don't decide
4. A destructive/irreversible action permissions didn't catch
5. About to arm the killer / enable auto_actuate / make a kill fire — never, surface
