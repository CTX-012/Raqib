# raqib — Video Production Plan (Phase 2, awaiting ratification)

**Status:** proposed · **Duration target:** 45–55 s · **Format:** 16:9 primary,
9:16 cutdown as follow-up · **Distribution:** README hero + docs site + social

This document translates the operator's storyboard work (the four `.dc.html`
design docs) into one build-ready plan. It fills in what those docs deliberately
left as agent decisions: the voice-over engine + voice choice, the asset
manifest against the files actually on disk, the per-scene honesty guardrails
carried over from `docs/MEDIA.md`, and the concrete build pipeline for Phase 3.

**Do not build until this plan is ratified.** The reshoot dependency (§9) is
load-bearing — the video's central claim ("raqib acts on its own") cannot be
honestly shipped from the recordings currently on disk.

---

## 1. Goals + audience

**Audience:** robotics + local-AI developers on shared GPU boxes. Self-educates,
auto-discounts hype. Same buyer the docs are written for — one tone across
every surface.

**One-sentence pitch:** raqib is one screen for every workload competing for
your GPU, and it can act.

**What the video must land:**
1. Recognize the pain in the first 3 seconds (VRAM contention on a shared GPU).
2. Show the tool doing the job — the 35-workload monitoring view.
3. Show the differentiator — the governor firing autonomously against a
   runaway workload, with everything else surviving.
4. Reassure on safety — off by default, four gates, web can't arm.
5. CTA — repo URL + install line.

**What the video must NOT do:**
- Overclaim performance (no unqualified benchmark numbers).
- Imply features not demonstrated in the recording (no "always" / "any").
- Show a manual `k`-press and imply autonomous action (a keypress on camera
  contradicts the whole safety story).

---

## 2. Format decisions (all reversible)

| Decision | Choice | Reason |
|---|---|---|
| **Duration** | 45–55 s (target 48 s) | Long enough for the four-gates diagram; short enough to hold on GitHub / social. |
| **Aspect** | 16:9 master, 1920×1080, H.264 | Universal. 9:16 cutdown for social is a follow-up render, not a re-shoot. |
| **Frame rate** | 30 fps | Matches the source recordings; no interpolation needed. |
| **Bitrate** | ~5 Mbps target, crf 20 | GitHub-friendly file size (~30 MB), no visible compression on the TUI. |
| **Colour** | Dark-only | The TUI + dashboard + docs are dark; no light-theme variant for video. |
| **Cadence** | Silent cut viable; VO + captions preferred | Auto-play on GitHub is silent — captions must carry the message on their own. |
| **SFX** | 3–4 subtle marks | Boot on title-in, tick on each gate, thud on SIGTERM, soft resolve on CTA. |
| **Music** | None (or a very sparse bed) | Developer audience penalises production overhead; silence + captions reads more credible. |

---

## 3. Storyboard (7 scenes, 48 s total)

Timing is scene-by-scene; total is the sum of the `dur` column. The `source`
column names the file the shot comes from — files verified to exist are marked
✓; files that require the reshoot are marked ⚠.

| # | tc | dur | scene | source | on-screen text |
|---|---|---|---|---|---|
| 1 | 0:00 | 3 s | **HOOK — cold-open on the VRAM number.** Extreme punch-in on the VRAM row of the live TUI, digits fill 1/3 of frame, number climbing. One red underline wipes in beneath on beat 2. Slow 3 % push-in throughout. | `docs/media/tui-workloads.png` ✓ (still, kenburns-panned) | "something just took 44% of your VRAM" |
| 2 | 0:03 | 6 s | **PROBLEM — pull back to the full workload list.** Zoom out from the VRAM row to the full AI-Workloads panel and keep pulling until the list runs off the bottom. Group headers sweep in order: LLM → Vision → Agent → ROS 2. Hold on a high-CPU row for a beat. | `rec_D_1602.mp4` ✓ (Recording 2026-08-14 16:02:13 — 35 workloads pan, 21 s) | "35 workloads · LLM · Vision · Agent · a full nav2 stack — one GPU" |
| 3 | 0:09 | 4 s | **TITLE.** Hard cut from footage to card. `raqib` types on in mono (cursor blinks once), tagline resolves letter-by-letter beneath. Faint VRAM-bar motif in the background. Hold two full seconds before cutting out. | Motion graphic (build in Phase 3) | "raqib — one pane of glass for everything fighting over your GPU. and it can act." |
| 4 | 0:13 | 8 s | **MONITOR — the whole view.** Slow Ken Burns across the TUI stills (vitals → top-processes → activity), then the web dashboard slides in bottom-right at 60 % scale. Three callout chips in sequence, never two at once. | `docs/media/tui-workloads.png` ✓, `docs/media/web-dashboard.png` ✓, `docs/media/activity-log.png` ✓ (composited) | "sorted by what it is · idle vs generating · tokens/sec · VRAM · thermals — an em dash where the number is unknowable, never a fake 0" |
| 5 | 0:21 | 10 s | **THE KILL — the governor fires on its own.** Beat 1: hold on the "Suggestion only — press k" state 1.5 s. Beat 2: whip down to Activity, punch in 1.6× on the SIGTERM line as it lands, bracket it. Beat 3: pink banner across the top gets a 0.2 s flash. Beat 4: pull out to show the still-running rows, green "others survive" pill lands. | ⚠ **RESHOOT REQUIRED.** Existing recordings are manual-k takes; the "acts on its own" claim requires an auto-kill take with no keypress on camera. (Fallback: use `docs/media/activity-log.png` as a still with kenburns + type-on of the SIGTERM line, labelled as an illustrative reconstruction.) | "SIGTERM OK auto · triggers=[vram] vram_pct=43.68 — every other workload untouched" |
| 6 | 0:31 | 11 s | **SAFETY — why that was safe.** Cut to the four-gates diagram. A token enters and clears allowlist → breach → sustain → rate-limit; SIGTERM lights. Immediately after, a second token labelled `rviz2` enters and is rejected at gate 1 (gate turns green, not red — "protected" not "denied"). Ends on the "off by default" stamp. | Motion graphic (build in Phase 3) | "allowlist → breach → sustained → rate-limit → SIGTERM · off by default, opt-in only, never armable from the web API" |
| 7 | 0:42 | 6 s | **CTA — end card.** The last live TUI frame freezes, desaturates 40 %, blurs 6 px, end card fades over. Repo line + install line stagger in. Hold 4 s. | Motion graphic + `docs/media/tui-workloads.png` ✓ (as freeze-frame background) | "github.com/CTX-012/Raqib · cargo build --release · Linux · beta · the governor is off until you turn it on" |

**Frame budget check:** 3 + 6 + 4 + 8 + 10 + 11 + 6 = **48 s**. ✓

**Cadence rhythm:** hook is punchy (3 s) → problem breathes (6 s) → title is a
beat (4 s) → monitor is the deep breath (8 s) → the kill is the payoff, longest
scene (10 s) → safety carries the diagram (11 s) → CTA is short and readable
(6 s). Tension → payoff → reassurance → action.

---

## 4. Voice-over script

One line per scene, matched to the scene's window. Phrasing chosen for
developer audience — flat, factual, no hype adjectives. Every claim is
grounded in what the frame visibly shows.

| # | scene | line (≈ words / target duration) |
|---|---|---|
| 1 | HOOK | *"Something on this box just took forty-four percent of your VRAM."* (10 w / ~3 s) |
| 2 | PROBLEM | *"Eight LLMs. Seven vision models. Two agents. A whole nav2 stack. Thirty-five workloads, one card."* (20 w / ~6 s) — pause — *"nvtop tells you it happened. Five terminals deep, with no idea which one to blame."* |
| 3 | TITLE | *"raqib is one pane of glass for everything fighting over your GPU."* (13 w / ~4 s) |
| 4 | MONITOR | *"It sorts them by what they are, and tells you which are actually working — idle models hold no VRAM, and raqib says so instead of guessing. Per-workload VRAM, CPU, thermals, tokens per second. When a number can't be measured, you get an em dash — not a fake zero."* (55 w / ~8 s) |
| 5 | KILL | *"Forty-three point six eight percent of VRAM, sustained past the threshold. The governor sent one SIGTERM — on its own — and logged exactly why. Nothing else on the box was touched."* (36 w / ~10 s) |
| 6 | SAFETY | *"Four gates before anything dies: allowlist, breach, sustained, rate-limit. It ships off. You arm it in a config file. The web API can't."* (30 w / ~11 s) |
| 7 | CTA | *"Open source, Linux, beta. It watches by default. Killing is your call."* (13 w / ~6 s) |

**Total: ~177 words.** At a natural narrator cadence (~2.5 words/s), that's
~71 s of speech — over the 48 s video. Trim targets if over on synth:
- Scene 2: drop "Five terminals deep" clause.
- Scene 4: drop "and tells you which are actually working" clause.
- Scene 5: drop the last sentence, let the visual carry it.

Fit-check every scene against its window on synthesis (§5). Never re-time
the video to fit the VO; always trim the line.

**Captions:** every line above is burned in as a caption (48 pt, DejaVu Sans,
white on 60 % black card, MarginV 52). VO is optional-audio; captions must
carry the message alone. Same wording — VO reads the caption verbatim so
they never diverge.

---

## 5. Voice-over engine + voice choice (agent recommendation)

**Recommendation: ElevenLabs, voice = "Adam" (default deep male neutral),
stability 0.5, similarity_boost 0.75, style 0.15.**

Rationale:
- The ElevenLabs key is already provisioned (0600, gitignored, read from env
  or `~/.elevenlabs_key` — never printed, never committed).
- Adam is neutral, professional, and reads technical copy without theatrical
  inflection — matches the developer-honest tone.
- Stability 0.5 keeps the read from wavering line-to-line without going flat.
- Style 0.15 = minimal delivery bias; developer audience penalises
  "advertisement voice".

**Alternates the operator may swap to:**

| Voice | Character | When it fits |
|---|---|---|
| **Bill** | Older, gravelly, calm | If Adam reads too "clean-corporate" — Bill adds weight. |
| **Rachel** | Warm professional female | If a female narrator is preferred; equally credible for tech copy. |
| **Josh** | Younger male, clear | If Adam reads too heavy for a 48-s piece — Josh is lighter. |

**Fallback: Piper offline.** If the ElevenLabs quota is hit or the key
misbehaves, fall back to Piper `en_US-lessac-medium` (already installed at
`~/.local/bin/piper`, model in `~/piper-models/` per convention). Piper is
free, deterministic, and audibly synthetic — usable in a pinch, but
ElevenLabs is the better choice for the front-page video.

**Silent-cut version.** Always produce a silent MP4 alongside the VO version
so GitHub's autoplay-muted README embed still lands the message via captions.

---

## 6. Motion graphics scenes to build (Phase 3)

Four scenes are motion graphics that don't exist as files yet. Each is
described in the operator's `Motion Graphics.dc.html` and included here as
concrete deliverables.

### 6a. Title card (scene 3, 4 s)

- Solid `#06070b` background, faint VRAM-bar motif behind (10 % opacity).
- `raqib` in mono (JetBrains Mono or Space Grotesk mono), 88 pt, `#6c9ef8`,
  types on left-to-right over 1.2 s. Cursor blinks once.
- Tagline in sans below, 20 pt, `#a8b0c4`, letter-resolves over 1.5 s.
- Hold 1.3 s.

### 6b. Four gates diagram (scene 6, 11 s)

- Four boxes in a row: `allowlist`, `breach`, `sustain`, `rate`, then a
  red `→ SIGTERM` cap on the right.
- Beat 1 (0–4 s): a token enters from the left, traverses each gate;
  each gate turns green (`#57d977`) as it passes. Hits SIGTERM; SIGTERM
  lights red (`#ff6b7a`).
- Beat 2 (4–8 s): a second token labelled `rviz2` enters. Gate 1
  (allowlist) intercepts it — the gate turns green (not red) with a
  "protected" chip. Token stops. This is the crucial safety-not-denial
  framing.
- Beat 3 (8–11 s): "off by default" stamp fades in bottom-centre.

### 6c. Callout overlays (scene 4 + 5, transparent-background PNG sequences)

- **VRAM arrow** — points at the VRAM number in scene 4, 1.5 s ease-in.
- **Kill-log bracket** — brackets the SIGTERM line in scene 5, 0.8 s
  snap-in with a subtle glow (2 px, `#ff6b7a` at 40 %).
- **"rviz2 survives" pill** — small rounded pill, `#57d977` border, 1.2 s
  scale-in bottom-right of scene 5.

### 6d. End card (scene 7, 6 s)

- Freeze-frame of `docs/media/tui-workloads.png` as background, blurred 6 px,
  desaturated 40 %.
- Overlay panel centre: `raqib` wordmark (36 pt, `#6c9ef8`), repo URL
  (`github.com/CTX-012/Raqib`, 16 pt), install line (`cargo build --release`,
  14 pt mono, `#8b93a7`).
- Reassurance line at bottom: *"the governor is off until you turn it on"*
  (14 pt italic, `#8b93a7`).
- Everything staggers in over 1.8 s, hold 4.2 s.

**Build stack option A (preferred): Remotion.** React-based, deterministic,
same toolchain as the gowning-vision playbook. Requires Node 20 in the
build environment.

**Build stack option B (fallback): pure ffmpeg + drawtext/subtitles.** No
Node dep, less polished. Sufficient if Remotion is a burden to introduce
into this repo.

**Recommendation:** Remotion, but built OUTSIDE this repo (in `~/video-studio`
or similar), rendering to MP4 that gets copied into `docs/` as a final asset.
No JS toolchain enters raqib itself.

---

## 7. Asset manifest — every file the video needs

Verified against what's in `~/Downloads/` and `docs/` on 2026-08-14.

| # | Asset | Source path (verified ✓ / needed ⚠) | Used in scene |
|---|---|---|---|
| 1 | `rec_D_1602.mp4` (35-workload monitoring pan, 21 s, 1766×798) | `~/Downloads/Recording 2026-08-14 160213.mp4` ✓ | 2 (PROBLEM), background material for 4 |
| 2 | `rec_A_1531.mp4` (short clip, 11 s, 1758×778) | `~/Downloads/Recording 2026-08-14 153144.mp4` ✓ | reserve — could underpin scene 1's VRAM close-up |
| 3 | `rec_C_1534.mp4` (long take, 58 s, 1862×900) | `~/Downloads/Recording 2026-08-14 153429.mp4` ✓ — but contains manual `k` kill; NOT usable for scene 5 | Reference only; scene 5 needs reshoot |
| 4 | `rec_B_1532.mp4` (4 s, 1752×700) | `~/Downloads/Recording 2026-08-14 153258.mp4` ✓ | reserve |
| 5 | TUI still | `docs/media/tui-workloads.png` ✓ | 1 (kenburns), 4 (main), 7 (freeze-frame) |
| 6 | Web dashboard still | `docs/media/web-dashboard.png` ✓ | 4 (bottom-right inset) |
| 7 | Activity log still | `docs/media/activity-log.png` ✓ | 4 (kenburns during activity beat), fallback for 5 |
| 8 | Title card MP4 | to build (§6a) ⚠ | 3 |
| 9 | Four-gates diagram MP4 | to build (§6b) ⚠ | 6 |
| 10 | Callout overlay PNG sequences | to build (§6c) ⚠ | 4, 5 |
| 11 | End card MP4 | to build (§6d) ⚠ | 7 |
| 12 | **Auto-kill reshoot** — clean take, no keypress on camera | ⚠ REQUIRED (see §9) | 5 |
| 13 | Voice-over WAVs (per scene) | to synthesize (§5) ⚠ | overlay, all scenes |
| 14 | SFX (title boot, gate tick ×4, SIGTERM thud, CTA resolve) | to source or synth ⚠ | 3, 6, 5, 7 |
| 15 | Caption SRT | derived from §4 script | overlay, all scenes |

**Final output files** (naming convention consistent with docs/MEDIA.md):

| File | Purpose |
|---|---|
| `docs/raqib-demo.mp4` | Master, 48 s, 16:9, VO + captions burned in |
| `docs/raqib-demo-silent.mp4` | Silent version, same frames, captions burned in |
| `docs/raqib-demo.srt` | Sidecar caption file for accessibility |
| `docs/raqib-demo-vertical.mp4` | 9:16 cutdown for social (follow-up, not gating Phase 3) |

---

## 8. Honesty guardrails (per scene, applying MEDIA.md rules)

The `docs/MEDIA.md` claim table extends to every frame of the video. This
section makes that explicit per scene.

| Scene | Allowed claim | NOT allowed |
|---|---|---|
| 1 HOOK | "44% VRAM" — matches the frame's visible number | Any claim the VRAM belongs to a specific attacker or class of workload without visible attribution |
| 2 PROBLEM | Whatever workload count is visible in the frame ("35 workloads" ✓ if 35 rows are on screen; else use the visible count) | "always" / "any GPU" — the pan shows this one dev box's mix |
| 3 TITLE | The tagline as written | "the ONLY tool that…" — tone-check against the docs' honesty stance |
| 4 MONITOR | Everything shown in the frame (categories, live values, em-dash on unknown) | "detects any framework" — raqib detects a specific list; don't imply it's exhaustive |
| 5 KILL | "SIGTERM ... source=Automated" is the recorded audit line, quotable verbatim if the reshoot shows it | "the biggest offender" — selection is deterministic (lowest-PID under rate cap), not necessarily biggest |
| 6 SAFETY | "off by default" ✓; "seven tripwire tests" ✓ (verifiable in `src/`); "four gates" ✓ | "guaranteed safe" / "never fails" — no absolutes |
| 7 CTA | Repo URL ✓; install line ✓; "beta" ✓ | Any performance claim in the outro |

**Meta-rule:** every frame the audience sees must be either (a) real footage
of `raqib`, (b) a still verified against MEDIA.md's claim table, or (c) a
motion-graphic that is unmistakably diagrammatic (the four-gates diagram is
obviously a diagram; nobody will confuse it with a UI). NO generative video
of anything.

---

## 9. What's blocked — the reshoot the video needs

**Scene 5 (THE KILL) cannot honestly be built from the current recordings.**

The existing `rec_C_1534.mp4` (58 s, the long take) contains a `k`-triggered
manual kill. Cutting it into scene 5 with the caption "the governor fires on
its own" is a lie — the keypress is on camera, and the story contradicts the
frame.

The operator's own `Storyboard.dc.html` calls this out under RESHOOT:

> The manual `k kill` take can't carry 0:23 — the whole claim is that raqib
> acted on its own, and a keypress on camera contradicts it.

**What the reshoot needs (one take, ~90 s of recording):**

1. Arm the governor in `~/.config/raqib/raqib.toml` — `auto_actuate = true`,
   `default_ai_action = "Kill"`. Set the VRAM threshold + sustain window low
   enough that ollama trips it within ~30 s of loading a model.
2. Have `rviz2` + the ROS 2 stack up + one `claude` agent — the survivors
   need to be visible on screen at the moment of the kill.
3. Terminal at large font, 1920×1080 or wider, no transparency, no other
   windows. Record the full screen at 60 fps if possible.
4. Run `raqib config check` once BEFORE hitting record — a still of that
   output is a bonus asset (proof of the pre-arm gate).
5. Start recording with the table idle, VRAM low. Hold **5 seconds** of calm.
6. Load the model from another (unrecorded) shell so no typing appears on
   camera. Let VRAM climb on screen — that climb is the 0:00 hook.
7. Hands off the keyboard from here. Let it breach, sustain, and fire.
   Keep rolling **10 seconds past** the kill so the Activity line AND the
   surviving rows are both on screen.
8. Restart ollama and let the table return to the opening state, still
   rolling — that tail is what makes any looped GIF of this seamless.

**Fallback if the reshoot doesn't happen:** Scene 5 becomes an animated
still. Use `docs/media/activity-log.png` as a kenburns background, type
the SIGTERM audit line in on beat, and add a caption labelling this as
"reconstruction from the audit log" so no visual is misleading. Less
punchy — the whole hero moment is a live capture in the strong version —
but it doesn't require the reshoot to ship.

---

## 10. Build pipeline (Phase 3, held for approval)

Once this plan is ratified, Phase 3 executes in this order:

1. **Approve VO engine + voice choice** — operator locks Adam or picks an
   alternate from §5.
2. **Synthesize VO** — per-scene WAVs via ElevenLabs API (key from env),
   fit-checked against each scene's window. Trim any over-window lines.
3. **Assemble VO timeline** — delay each clip to its scene offset,
   loudness-normalize to −16 LUFS, generate SRT sidecar from the script.
4. **Build motion graphics** — title card, gates diagram, callouts, end
   card. Rendered outside the repo (Remotion or ffmpeg), MP4 outputs
   copied into a working dir.
5. **Integrate real footage** — trim `rec_D_1602.mp4` for scene 2, use
   the ratified auto-kill reshoot for scene 5, use the three stills with
   kenburns pans for scene 4.
6. **Mux + captions** — assemble scenes end-to-end, mux VO track, burn
   captions.
7. **Verify** — every honesty rule from §8 spot-checked against the
   rendered frames (not just the plan). File under 40 MB. Plays clean on
   GitHub, VLC, and a mobile browser.
8. **Ship** — `docs/raqib-demo.mp4` + `docs/raqib-demo-silent.mp4` +
   `docs/raqib-demo.srt` land in `docs/`. README embeds it (GitHub renders
   MP4 inline). Commit + push.

**Not shipping until:** every honesty rule is satisfied AND the reshoot
either lands (strong version) or the fallback labelling is applied
explicitly (weak version).

---

## 11. What I need from the operator to move to Phase 3

1. **Approve or amend this plan.** Anything you disagree with here —
   scene order, timing, wording, voice choice — call out and I'll revise.
2. **Voice choice.** Lock ElevenLabs / Adam, or swap to another voice, or
   fall back to Piper.
3. **The reshoot** (or explicit approval of the fallback). If the reshoot
   is happening, capture per §9 and drop the file in `~/Downloads/` with
   any filename — I'll pick it up.
4. **CTA URL final** — the plan uses `github.com/CTX-012/Raqib`; confirm
   or swap for a shortened URL (like `raqib.dev` if that ever exists).
5. **Any additions.** If there's a claim you want the video to make that
   isn't in §4 (e.g. "1.3 KLOC", "1381 tests"), pass it in — every claim
   needs a matching frame source, and I'll fit it in the storyboard.

Once ratified, Phase 3 execution is estimated at **~2–4 hours** end-to-end
(VO synth + motion graphics + integration + mux + verify + push), assuming
the reshoot is in hand.
