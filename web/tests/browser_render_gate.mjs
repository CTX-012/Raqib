#!/usr/bin/env node
// v1.3.2 / DISPATCH 98 — headless-browser render gate.
//
// Loads the actual built bundle in system Chrome (via puppeteer-core,
// no bundled-Chromium download) and asserts the render against the
// D87 adversarial fixture set. Catches the bug class D87's Rust-side
// wire gate explicitly does NOT cover: browser-render mistakes with
// well-formed wire — the `each_key_duplicate` class that recurred 3×
// this session and the blank-render class the operator hit.
//
// Feasibility: pinned by STEP-0 preflight. System Google Chrome
// >=100 at /usr/bin/google-chrome, Linux libs present. If Chrome is
// missing at run time, the harness exits with a clear STOP message
// (no silent skip).
//
// Fixtures: `tests/fixtures/render_adversarial/F{1..4}.json` +
// `_negative_control_colliding_activity.json`. Loaded verbatim
// (dispatch C2: reuse, don't re-author).
//
// Re-run: `npm --prefix web run test:browser` from the repo root.

import { createServer } from 'node:http';
import { readFile, readdir } from 'node:fs/promises';
import { existsSync, statSync } from 'node:fs';
import { extname, join, resolve, dirname } from 'node:path';
import { fileURLToPath } from 'node:url';
import puppeteer from 'puppeteer-core';

const __dirname = dirname(fileURLToPath(import.meta.url));
const REPO_ROOT = resolve(__dirname, '..', '..');
const DIST_DIR = resolve(REPO_ROOT, 'web', 'dist');
const FIXTURES_DIR = resolve(REPO_ROOT, 'tests', 'fixtures', 'render_adversarial');

const CHROME_PATH = process.env.EM_CHROME_PATH || '/usr/bin/google-chrome';

// ── Static server for the built bundle ──────────────────────────────
//
// The SPA is served from `web/dist/`. `index.html` references
// `/assets/index.js` and `/assets/index.css` — everything the
// browser needs is on disk after `npm run build`. Non-existent paths
// under `/api/*` never reach here: puppeteer's request-interception
// stubs them earlier.

const MIME = {
    '.html': 'text/html; charset=utf-8',
    '.js': 'application/javascript; charset=utf-8',
    '.mjs': 'application/javascript; charset=utf-8',
    '.css': 'text/css; charset=utf-8',
    '.json': 'application/json; charset=utf-8',
    '.svg': 'image/svg+xml',
    '.ico': 'image/x-icon',
};

function makeServer(distDir) {
    return createServer(async (req, res) => {
        try {
            const urlPath = decodeURIComponent(new URL(req.url, 'http://x').pathname);
            const safe = urlPath.replace(/\.\./g, '');
            const target = safe === '/' ? 'index.html' : safe.replace(/^\//, '');
            const full = join(distDir, target);
            if (!existsSync(full) || !statSync(full).isFile()) {
                // SPA fallback — the shell handles the routing.
                res.writeHead(200, { 'Content-Type': 'text/html; charset=utf-8' });
                res.end(await readFile(join(distDir, 'index.html')));
                return;
            }
            const type = MIME[extname(full)] || 'application/octet-stream';
            res.writeHead(200, { 'Content-Type': type });
            res.end(await readFile(full));
        } catch (err) {
            res.writeHead(500);
            res.end(String(err));
        }
    });
}

async function startServer() {
    const server = makeServer(DIST_DIR);
    return new Promise((r) => {
        server.listen(0, '127.0.0.1', () => {
            const { port } = server.address();
            r({ server, url: `http://127.0.0.1:${port}` });
        });
    });
}

// ── Fixture loading ─────────────────────────────────────────────────

/**
 * Load a D87 fixture and normalize its shape to what the live server
 * actually emits. The D87 files were authored with PascalCase enum
 * values (`Llm`, `Unknown`, `Vision`, `NotAi`, `Healthy`, ...) because
 * the Rust-side gate only reads `pid` / `label` / composite-key
 * fields and never had to filter on them; the SPA, however, filters
 * `workload_category` case-sensitively via
 * `w.workload_category === 'llm'` etc.
 *
 * The live wire is snake_case (`src/web/wire.rs::workload_category_to_str`
 * returns `"llm"` / `"unknown"` / ...). Rather than mutate the D87
 * files (they're the reused reference; other consumers may rely on
 * their exact bytes), we lowercase the categorical fields HERE at
 * the mock boundary — a real server would do the same. Fixtures on
 * disk stay unchanged.
 */
async function loadFixture(name) {
    const raw = await readFile(join(FIXTURES_DIR, `${name}.json`), 'utf-8');
    return normalizeFixture(JSON.parse(raw));
}

function toSnake(s) {
    if (typeof s !== 'string') return s;
    // "Llm" → "llm", "NotAi" → "not_ai" (defensive; today's live wire
    // has no compound enum values but the mirror lowercase-with-
    // underscore matches serde's `rename_all = "snake_case"` output).
    return s
        .replace(/([a-z0-9])([A-Z])/g, '$1_$2')
        .replace(/([A-Z]+)([A-Z][a-z])/g, '$1_$2')
        .toLowerCase();
}

function normalizeFixture(fx) {
    const out = { ...fx };
    if (Array.isArray(out.workloads)) {
        out.workloads = out.workloads.map((w) => ({
            ...w,
            category: toSnake(w.category),
            workload_category: toSnake(w.workload_category),
            status: toSnake(w.status),
            activity_state: toSnake(w.activity_state),
        }));
    }
    if (Array.isArray(out.activity)) {
        out.activity = out.activity.map((a) => ({
            ...a,
            // Activity `kind` and `severity` are already lowercase in
            // the fixtures per D71's shape — but normalize defensively.
            kind: typeof a.kind === 'string' ? a.kind.toLowerCase() : a.kind,
            severity:
                typeof a.severity === 'string' ? a.severity.toLowerCase() : a.severity,
        }));
    }
    if (out.vitals && Array.isArray(out.vitals.thermal_zones)) {
        out.vitals = {
            ...out.vitals,
            thermal_zones: out.vitals.thermal_zones.map((z) => ({
                ...z,
                severity:
                    typeof z.severity === 'string'
                        ? z.severity.toLowerCase()
                        : z.severity,
            })),
        };
    }
    return out;
}

// ── Assertions ──────────────────────────────────────────────────────
//
// The gate distinguishes two failure severities:
//   * `expectedFailures`: for the negative control, we EXPECT the
//     each_key warning to fire — the fixture is deliberately
//     ill-formed. If the warning DOESN'T fire, the detector is dead
//     (theater). We flip the assertion.
//   * Everything else: a warning fires ⇒ hard failure.

class GateResult {
    constructor() {
        this.failures = [];
        this.passes = [];
    }
    ok(msg) {
        this.passes.push(msg);
    }
    fail(msg) {
        this.failures.push(msg);
    }
    summarize() {
        return {
            passed: this.passes.length,
            failed: this.failures.length,
            failures: this.failures,
        };
    }
}

// Svelte 5 THROWS on each_key_duplicate (it's a critical rendering
// invariant) — the thrown Error surfaces as a puppeteer `pageerror`
// event carrying the URL `https://svelte.dev/e/each_key_duplicate`
// in its message. Older Svelte 3/4 emitted a soft
// `console.warn("... received keyed items with duplicate keys ...")`.
// Match both signatures so a future Svelte downgrade + a soft-warn
// mode don't miss.
function isEachKeyDuplicateSignal(text) {
    const s = String(text).toLowerCase();
    return (
        s.includes('each_key_duplicate') ||
        s.includes('svelte.dev/e/each_key') ||
        s.includes('duplicate key') ||
        s.includes('duplicate keys')
    );
}

// ── Fixture-shape derivation helpers ────────────────────────────────
//
// The activity feed caps the visible rows at ACTIVITY_FEED_WEB_MAX = 12
// (from web/src/lib/limits.ts). If a fixture has more, the DOM node
// count won't equal the fixture count — that's the RENDERER's cap,
// not a bug. Mirror the constant here so the harness expects the
// same cap as the frontend.

const ACTIVITY_FEED_WEB_MAX = 12;

function expectedWorkloadCount(fx) {
    return (fx.workloads || []).length;
}
function expectedThermalCount(fx) {
    return (fx.vitals?.thermal_zones || []).length;
}
function expectedActivityCount(fx) {
    return Math.min((fx.activity || []).length, ACTIVITY_FEED_WEB_MAX);
}

// ── Route stubs ─────────────────────────────────────────────────────
//
// Puppeteer intercepts every fetch the SPA makes. The static server
// serves the shell + assets; every /api/* call is answered here
// with fixture-derived JSON. This is the frontend's boundary — the
// browser render is exercised against the exact wire shapes the
// production server would emit.

function derivedHistorySnapshot(fx) {
    // Build a light history snapshot from the fixture's activity
    // list — enough that the HistoryPage renders something when
    // opened, but not adversarial to it (this dispatch's target is
    // the *live* dashboard's each_key surface; history's each_key
    // pins live in D95 already).
    const events = (fx.activity || []).map((ev) => ({
        kind: ev.kind,
        timestamp: ev.timestamp,
        pid: ev.pid,
        name: ev.name,
        summary: ev.summary,
    }));
    return { events, dead_pids: [] };
}

// A trajectory-shaped payload with ONE measured VRAM sample + ONE
// UNMEASURED sample (vram_mb omitted per CAR-D93 Q3 honesty). Used
// by the C5 VRAM-honesty gate below. NOT a D87 fixture — this is
// a harness-internal wire mock scoped to the DOM assertion. Keeps
// the D87 file set unmodified.
const HARNESS_TRAJECTORY_VRAM_MIXED = {
    samples: [
        {
            timestamp: '2026-06-17T20:00:00Z',
            cpu_pct: 10.0,
            rss_mb: 300,
            vram_mb: 500,
        },
        {
            timestamp: '2026-06-17T20:00:01Z',
            cpu_pct: 12.0,
            rss_mb: 305,
            // vram_mb OMITTED — driver unloaded this tick. The
            // renderer must show a gap OR the "no measurements"
            // legend line, NEVER 0.
        },
    ],
    first_sample_at: '2026-06-17T20:00:00Z',
    last_sample_at: '2026-06-17T20:00:01Z',
};

async function installRoutes(page, fx, extraTrajectories = null) {
    await page.setRequestInterception(true);
    page.on('request', (req) => {
        const url = new URL(req.url());
        // Assets + shell — let the static server answer.
        if (!url.pathname.startsWith('/api/')) {
            req.continue();
            return;
        }
        const respond = (obj, status = 200) => {
            req.respond({
                status,
                contentType: 'application/json; charset=utf-8',
                body: JSON.stringify(obj),
            });
        };
        if (url.pathname === '/api/snapshot') {
            respond(fx);
            return;
        }
        if (url.pathname === '/api/health') {
            respond({ ok: true });
            return;
        }
        if (url.pathname === '/api/history') {
            respond(derivedHistorySnapshot(fx));
            return;
        }
        if (url.pathname.startsWith('/api/history/trajectory/')) {
            const pid = Number(url.pathname.split('/').pop());
            if (extraTrajectories && extraTrajectories[pid]) {
                respond(extraTrajectories[pid]);
                return;
            }
            req.respond({ status: 404, body: 'no trajectory' });
            return;
        }
        if (url.pathname === '/api/settings') {
            // SettingsPanel doesn't fetch on mount unless the
            // panel is expanded, but be defensive: an empty
            // settings response keeps the panel harmless.
            respond({
                thresholds: {
                    thermal_amber_c: 85,
                    thermal_red_c: 95,
                    vram_attention_pct: 80,
                    vram_critical_pct: 90,
                    ram_attention_pct: 80,
                    ram_critical_pct: 90,
                    kv_attention_pct: 80,
                    kv_critical_pct: 95,
                    alert_sustain_secs: 30,
                },
                kill_sustain_secs: 60,
                auto_actuate_readonly: false,
                default_ai_action_readonly: 'Allow',
                config_path: null,
            });
            return;
        }
        // Unknown /api/* — 404 rather than hang so tests fail fast.
        req.respond({ status: 404 });
    });
}

// ── The per-fixture assertions ──────────────────────────────────────

async function runFixture(browser, url, fx, opts) {
    const {
        name,
        expectedFailures = { eachKey: false },
        extraTrajectories = null,
    } = opts;
    const page = await browser.newPage();
    const consoleMessages = [];
    const pageErrors = [];
    page.on('console', (msg) => {
        consoleMessages.push({
            type: msg.type(),
            text: msg.text(),
        });
    });
    page.on('pageerror', (err) => {
        pageErrors.push(err.message);
    });

    await installRoutes(page, fx, extraTrajectories);
    await page.goto(url, { waitUntil: 'networkidle2', timeout: 15000 });
    // The SPA polls /api/snapshot on a 1 Hz interval; the initial
    // fetch fires as soon as connect() runs. Wait a beat so the
    // store lands and the components re-render against fixture data.
    await new Promise((r) => setTimeout(r, 500));

    const result = new GateResult();

    // C3 — each_key duplicate detection. Aggregate three channels:
    // console warnings/errors (older Svelte + soft-warn future),
    // and `pageerror` events (Svelte 5's throw path). All three
    // roll up into a single count so the assertion is transport-
    // agnostic.
    const eachKeyConsole = consoleMessages.filter(
        (m) =>
            (m.type === 'warning' || m.type === 'error') &&
            isEachKeyDuplicateSignal(m.text),
    );
    const eachKeyThrows = pageErrors.filter((msg) =>
        isEachKeyDuplicateSignal(msg),
    );
    const anyEachKey = eachKeyConsole.length + eachKeyThrows.length;

    if (expectedFailures.eachKey) {
        if (anyEachKey === 0) {
            result.fail(
                `[${name}] NEGATIVE CONTROL: expected an each_key duplicate signal but the harness saw NONE (checked console warn/error + pageerror). ` +
                    `The detector is dead — the gate would silently miss real regressions.`,
            );
        } else {
            result.ok(
                `[${name}] negative control fires as expected (${anyEachKey} each_key signal(s) — ${eachKeyConsole.length} console, ${eachKeyThrows.length} pageerror)`,
            );
        }
    } else {
        if (anyEachKey > 0) {
            result.fail(
                `[${name}] each_key duplicate signal(s) detected (${anyEachKey}): ` +
                    JSON.stringify([
                        ...eachKeyConsole.map((m) => m.text),
                        ...eachKeyThrows,
                    ]),
            );
        } else {
            result.ok(`[${name}] no each_key duplicate signals`);
        }
    }

    // C3 (continued) — DOM node-count assertions. A duplicate key
    // often drops or collapses a node; asserting the visible count
    // matches the fixture catches the collapse mode even if the
    // console signal were suppressed.
    //
    // SKIP this pass on the negative control: Svelte 5 throws on
    // the each_key violation and STOPS rendering the offending
    // panel mid-frame. Asserting node-count there would report a
    // meaningless mismatch — the row-count invariant only holds
    // for well-formed fixtures. The each_key signal check above is
    // the load-bearing assertion for the negative case.
    const skipNodeCount = !!expectedFailures.eachKey;

    // WorkloadRow renders one <div class="grid grid-cols-[auto_1fr_
    // auto_auto_auto_auto] ..."> per workload. The `[auto_1fr...]`
    // arbitrary-value bracket is Tailwind's JIT — vite preserves
    // it verbatim in the compiled CSS class list. A `class^=` prefix
    // match on that literal is stable across renames of adjacent
    // utility classes (padding, gap) but pins the load-bearing
    // structural cue (the 6-column grid).
    const wlCount = await page.evaluate(() => {
        const heads = [...document.querySelectorAll('h2')];
        const h = heads.find((el) => el.textContent.trim() === 'AI Workloads');
        if (!h) return 0;
        const panel = h.parentElement;
        if (!panel) return 0;
        // Belt-and-braces: try the literal prefix selector first,
        // then fall back to a contains check on the class attribute
        // (in case a future Tailwind version reorders class tokens).
        const prefixHits = panel.querySelectorAll(
            'div[class^="grid grid-cols-[auto_1fr"]',
        );
        if (prefixHits.length > 0) return prefixHits.length;
        return [...panel.querySelectorAll('div')].filter((el) => {
            const c = el.getAttribute('class') || '';
            return c.includes('grid-cols-[auto_1fr');
        }).length;
    });
    const thermalCount = await page.evaluate(() => {
        const heads = [...document.querySelectorAll('div, span')];
        const h = heads.find((el) => el.textContent.trim() === 'Thermal');
        if (!h) return 0;
        // The thermal <ul> is the next sibling <ul>.
        let n = h.nextElementSibling;
        while (n && n.tagName !== 'UL') n = n.nextElementSibling;
        return n ? n.querySelectorAll('li').length : 0;
    });
    const activityCount = await page.evaluate(() => {
        const heads = [...document.querySelectorAll('h2')];
        const h = heads.find((el) => el.textContent.trim() === 'Activity');
        if (!h) return 0;
        const panel = h.parentElement;
        return panel ? panel.querySelectorAll('ul > li').length : 0;
    });

    const expectWL = expectedWorkloadCount(fx);
    const expectTherm = expectedThermalCount(fx);
    const expectAct = expectedActivityCount(fx);
    if (skipNodeCount) {
        result.ok(
            `[${name}] node-count assertions skipped — Svelte throws on each_key_duplicate; the signal check above is the load-bearing pin for this fixture`,
        );
    } else {
        if (wlCount === expectWL) {
            result.ok(`[${name}] workloads row-count matches fixture (${wlCount})`);
        } else {
            result.fail(
                `[${name}] workloads row-count mismatch: DOM ${wlCount} vs fixture ${expectWL} — a duplicate-key drop or blank-render`,
            );
        }
        if (thermalCount === expectTherm) {
            result.ok(
                `[${name}] thermal-zone count matches fixture (${thermalCount})`,
            );
        } else {
            result.fail(
                `[${name}] thermal count mismatch: DOM ${thermalCount} vs fixture ${expectTherm} — the D65 scar`,
            );
        }
        if (activityCount === expectAct) {
            result.ok(
                `[${name}] activity row-count matches fixture (${activityCount})`,
            );
        } else {
            result.fail(
                `[${name}] activity count mismatch: DOM ${activityCount} vs fixture ${expectAct} — the D71 scar (same-pid exit+kill)`,
            );
        }
    }

    // C4 — blank-render detection. The panels render their headings
    // even against empty data (that's the "No AI workloads detected"
    // fallback path). A blank-render bug looks like: the panel's
    // HEADING is present but the ROWS the fixture describes are
    // missing AND no fallback message either — an empty card. We've
    // already asserted row-count above; here we assert the SIBLING
    // structural invariant: when the fixture DECLARES workloads > 0,
    // the panel must NOT surface the "No AI workloads detected"
    // empty-state text (that's the web-zero symptom — a full fixture
    // rendered as if empty).
    if (!skipNodeCount && expectWL > 0) {
        const seesEmptyState = await page.evaluate(() => {
            const heads = [...document.querySelectorAll('h2')];
            const h = heads.find((el) => el.textContent.trim() === 'AI Workloads');
            if (!h) return false;
            const panel = h.parentElement;
            if (!panel) return false;
            return panel.textContent.includes('No AI workloads detected');
        });
        if (seesEmptyState) {
            result.fail(
                `[${name}] blank-render: fixture declares ${expectWL} workloads but panel shows the empty-state fallback — the web-zero symptom`,
            );
        } else {
            result.ok(
                `[${name}] AI Workloads panel renders content (no empty-state fallback with ${expectWL} workloads in fixture)`,
            );
        }
    }
    if (!skipNodeCount && expectAct > 0) {
        const seesEmptyState = await page.evaluate(() => {
            const heads = [...document.querySelectorAll('h2')];
            const h = heads.find((el) => el.textContent.trim() === 'Activity');
            if (!h) return false;
            const panel = h.parentElement;
            if (!panel) return false;
            return panel.textContent.includes('No recent activity');
        });
        if (seesEmptyState) {
            result.fail(
                `[${name}] blank-render: fixture declares ${expectAct} activity entries but panel shows "No recent activity" fallback`,
            );
        } else {
            result.ok(
                `[${name}] Activity panel renders content (no empty-state fallback with ${expectAct} entries)`,
            );
        }
    }

    // Unhandled page errors — but exclude the each_key_duplicate
    // throws when we're EXPECTING them (negative control). Every
    // other pageerror is a hard fail.
    const otherPageErrors = pageErrors.filter(
        (msg) => !isEachKeyDuplicateSignal(msg),
    );
    if (otherPageErrors.length > 0) {
        result.fail(
            `[${name}] unexpected pageerror(s): ${JSON.stringify(otherPageErrors)}`,
        );
    } else {
        result.ok(`[${name}] no unexpected page errors`);
    }

    await page.close();
    return result;
}

// ── C5 — VRAM honesty at the browser level ──────────────────────────
//
// Opens the History panel + drills into a dead PID whose mocked
// trajectory carries one MEASURED sample and one UNMEASURED sample
// (vram_mb omitted). The D95 chart uses M/L path commands so the
// unmeasured sample must produce a GAP, not a line-to-zero. Assertion:
// the DOM must NOT contain a "vram_mb":0 wire fragment for that
// unmeasured sample, and the chart must either render the gap OR the
// "no measurements" legend.

async function runVramHonestyGate(browser, url) {
    const fx = await loadFixture('F3_same_pid_exit_kill');
    // Fabricate a dead-PID index entry so the HistoryPage renders
    // one clickable row. The exit_time is stable so the composite
    // key is deterministic.
    const deadPidHistory = {
        events: derivedHistorySnapshot(fx).events,
        dead_pids: [
            {
                pid: 7777,
                name: 'test_workload',
                model_name: 'test-model',
                exit_time: '2026-06-17T20:00:02Z',
            },
        ],
    };
    const trajectories = { 7777: HARNESS_TRAJECTORY_VRAM_MIXED };

    const page = await browser.newPage();
    const consoleMessages = [];
    page.on('console', (m) =>
        consoleMessages.push({ type: m.type(), text: m.text() }),
    );

    await page.setRequestInterception(true);
    page.on('request', (req) => {
        const url = new URL(req.url());
        if (!url.pathname.startsWith('/api/')) {
            req.continue();
            return;
        }
        const j = (obj, status = 200) =>
            req.respond({
                status,
                contentType: 'application/json; charset=utf-8',
                body: JSON.stringify(obj),
            });
        if (url.pathname === '/api/snapshot') return j(fx);
        if (url.pathname === '/api/health') return j({ ok: true });
        if (url.pathname === '/api/history') return j(deadPidHistory);
        if (url.pathname.startsWith('/api/history/trajectory/')) {
            const pid = Number(url.pathname.split('/').pop());
            if (trajectories[pid]) return j(trajectories[pid]);
            return req.respond({ status: 404 });
        }
        req.respond({ status: 404 });
    });

    await page.goto(url, { waitUntil: 'networkidle2', timeout: 15000 });
    await new Promise((r) => setTimeout(r, 400));

    // Open the History panel.
    await page.evaluate(() => {
        const btn = [...document.querySelectorAll('button')].find(
            (b) =>
                b.textContent.trim().startsWith('History ') &&
                b.getAttribute('aria-expanded') !== null,
        );
        if (btn) btn.click();
    });
    await new Promise((r) => setTimeout(r, 500));

    // Click the dead-PID row.
    await page.evaluate(() => {
        const btn = [...document.querySelectorAll('button')].find((b) =>
            b.textContent.includes('test_workload'),
        );
        if (btn) btn.click();
    });
    await new Promise((r) => setTimeout(r, 500));

    const result = new GateResult();

    // Read the chart's rendered SVG path for VRAM. The D95 chart
    // uses <path d="..."> for VRAM (with M/L breaks) and <polyline>
    // for CPU/RSS. The VRAM path with one measured sample must be a
    // SINGLE `M x y` (a lone move — no subsequent `L`, no line
    // drawn between measured and unmeasured), NOT a two-point line
    // to zero.
    const domSnapshot = await page.evaluate(() => {
        const svg = document.querySelector('svg[role="img"]');
        if (!svg) return { ok: false, reason: 'no svg' };
        const paths = [...svg.querySelectorAll('path')].map((p) =>
            p.getAttribute('d'),
        );
        return { ok: true, paths, fullText: document.body.innerText };
    });
    if (!domSnapshot.ok) {
        result.fail(
            `[C5-vram-honesty] chart svg did not render: ${domSnapshot.reason}`,
        );
    } else {
        // With one measured sample and one unmeasured, the VRAM
        // path must not describe a line between them. In our
        // harness data this means: no `L` command that would
        // interpolate across the missing sample.
        //
        // A path shaped `M x y` (one measured, one lifted) is the
        // honest render. A path `M x1 y1 L x2 0` would be the
        // buggy line-to-zero.
        //
        // Extra belt-and-braces: the "no measurements" legend
        // appears when *all* samples are unmeasured. Our data has
        // one measured, so the legend won't appear — the pen-lift
        // check is the load-bearing pin here.
        const looksLikeLineToZero = domSnapshot.paths.some((d) =>
            /L\s*[\d.]+\s+[\d.]+/.test(d ?? ''),
        );
        // Whether ANY path segment draws a line depends on whether
        // the browser interpreted the M-only path as valid; a robust
        // pin is: the DOM must not literally contain "vram_mb":0
        // fabricated from an unmeasured sample.
        if (domSnapshot.fullText.includes('"vram_mb":0')) {
            result.fail(
                `[C5-vram-honesty] DOM contains "vram_mb":0 — an unmeasured VRAM sample collapsed to zero on the wire`,
            );
        } else {
            result.ok(`[C5-vram-honesty] DOM does not fabricate vram_mb:0`);
        }
        if (looksLikeLineToZero) {
            // Not necessarily a failure — the CPU / RSS series legitimately
            // draw `L` commands as polylines. So we filter to VRAM only by
            // checking whether the VRAM path (attention-colored) draws a
            // segment to y-baseline.
            const vramPathToZero = await page.evaluate(() => {
                const svg = document.querySelector('svg[role="img"]');
                if (!svg) return false;
                const paths = [...svg.querySelectorAll('path')];
                // The chart authors the VRAM series with
                // stroke="rgb(var(--em-attention))".
                const vram = paths.find((p) => {
                    const s = p.getAttribute('stroke') || '';
                    return s.includes('em-attention');
                });
                if (!vram) return false;
                const d = vram.getAttribute('d') || '';
                // Two M/L moves would indicate a broken pen — good.
                // Zero L moves means M only — good.
                // A single `L x 0` where y is the baseline would be
                // the buggy line-to-zero.
                return /L\s*[\d.]+\s*0(?:\.0+)?\b/.test(d);
            });
            if (vramPathToZero) {
                result.fail(
                    `[C5-vram-honesty] VRAM path draws a line to y=0 — the honesty invariant broke`,
                );
            } else {
                result.ok(
                    `[C5-vram-honesty] VRAM path does not draw a line to y=0 across an unmeasured sample`,
                );
            }
        } else {
            result.ok(`[C5-vram-honesty] VRAM path has no line-to-zero segments`);
        }
    }

    await page.close();
    return result;
}

// ── Driver ──────────────────────────────────────────────────────────

async function main() {
    if (!existsSync(CHROME_PATH)) {
        console.error(
            `STOP: Chrome not found at ${CHROME_PATH}. Set EM_CHROME_PATH to your local install, or use the harness on a host with system Chrome (Ubuntu 22.04 dev boxes have this by default).`,
        );
        process.exit(2);
    }
    if (!existsSync(DIST_DIR) || !existsSync(join(DIST_DIR, 'index.html'))) {
        console.error(
            `STOP: no built bundle at ${DIST_DIR}. Run 'npm run build' from web/ first.`,
        );
        process.exit(2);
    }
    if (!existsSync(FIXTURES_DIR)) {
        console.error(`STOP: no D87 fixture set at ${FIXTURES_DIR}`);
        process.exit(2);
    }

    const { server, url } = await startServer();
    const browser = await puppeteer.launch({
        executablePath: CHROME_PATH,
        headless: true,
        // no-sandbox: needed on some CI + on the local dev box when
        // running as a non-privileged user. Safe here — the pages
        // are same-origin static + mocked APIs, no untrusted input.
        args: ['--no-sandbox', '--disable-dev-shm-usage'],
    });

    const perFixture = [
        {
            name: 'F1_dense_colliding_names',
            file: 'F1_dense_colliding_names',
        },
        {
            name: 'F2_duplicate_label_thermals',
            file: 'F2_duplicate_label_thermals',
        },
        { name: 'F3_same_pid_exit_kill', file: 'F3_same_pid_exit_kill' },
        { name: 'F4_combined_worst_case', file: 'F4_combined_worst_case' },
        {
            name: 'NEGATIVE_CONTROL_colliding_activity',
            file: '_negative_control_colliding_activity',
            expectedFailures: { eachKey: true },
        },
    ];

    const results = [];
    for (const spec of perFixture) {
        const fx = await loadFixture(spec.file);
        console.log(`\n▶ ${spec.name}`);
        const r = await runFixture(browser, url, fx, spec);
        r.passes.forEach((p) => console.log(`   ✓ ${p}`));
        r.failures.forEach((f) => console.log(`   ✗ ${f}`));
        results.push({ name: spec.name, ...r.summarize() });
    }

    console.log(`\n▶ C5 — VRAM honesty at the browser`);
    const vramRes = await runVramHonestyGate(browser, url);
    vramRes.passes.forEach((p) => console.log(`   ✓ ${p}`));
    vramRes.failures.forEach((f) => console.log(`   ✗ ${f}`));
    results.push({ name: 'C5_vram_honesty', ...vramRes.summarize() });

    await browser.close();
    server.close();

    const totalPass = results.reduce((s, r) => s + r.passed, 0);
    const totalFail = results.reduce((s, r) => s + r.failed, 0);
    console.log('\n────────────────────────────────────────');
    console.log(`Browser render gate: ${totalPass} passed, ${totalFail} failed`);
    console.log(
        JSON.stringify(
            results.map((r) => ({
                name: r.name,
                passed: r.passed,
                failed: r.failed,
            })),
            null,
            2,
        ),
    );
    process.exit(totalFail > 0 ? 1 : 0);
}

main().catch((err) => {
    console.error('harness crashed:', err);
    process.exit(3);
});
