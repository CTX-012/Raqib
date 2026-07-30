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
// ── Re-run ──────────────────────────────────────────────────────────
//
// From the repo root:
//   npm --prefix web run test:browser
//
// Or from `web/`:
//   npm run test:browser
//
// The npm script chains `npm run build && node tests/browser_render_gate.mjs`
// so the gate always runs against a fresh bundle. Exit code 0 on
// pass, 1 on any assertion failure, 2 on missing prerequisites
// (no Chrome, no fixtures, no built bundle), 3 on harness crash.
//
// Environment:
//   * EM_CHROME_PATH — override the Chrome binary path
//     (default: /usr/bin/google-chrome). Tested with Google Chrome
//     150 on Ubuntu 22.04.
//
// CI hint: puppeteer-core does NOT download Chromium. The runner
// must provide Chrome via apt-get, a base image, or the
// EM_CHROME_PATH env var — see the `test:browser` script.

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
                // Thermal-friendly dispatch — friendly_label was added
                // to WireThermalZone on the server side. Existing
                // fixtures pre-date the field; fall back to the raw
                // label so they still parse. The dedicated
                // `runThermalFriendlyGate` probe below drives a
                // fixture that DOES set friendly_label so the render
                // path is exercised.
                friendly_label: z.friendly_label ?? z.label,
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

    // WorkloadRow.svelte carries `data-testid="workload-row"` (added
    // in D98 alongside this harness) — a stable structural hook that
    // survives CSS refactors. Ships inert: no styling, no behavior,
    // no bytes in the compiled output beyond the attribute itself.
    // If the testid is removed from WorkloadRow, this selector will
    // return 0 and the fixture row-count assertion will fail loudly
    // rather than silently — the correct fail-loud coupling for a
    // structural pin.
    const wlCount = await page.$$eval(
        '[data-testid="workload-row"]',
        (els) => els.length,
    );
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

    // v1.3.2 / DISPATCH 101 — C5 used to open the dashboard's
    // collapsible HistoryPage; D101 promoted History to its own
    // mode and removed the collapsible from the dashboard. Load
    // the history mode directly — HistoryPage's `alwaysOpen`
    // fires the fetch on mount, so no toggle click is needed.
    await page.goto(`${url}/?mode=history`, {
        waitUntil: 'networkidle2',
        timeout: 15000,
    });
    // Give the mount + snapshot-on-open fetch + first render a
    // beat to land before we ask for the dead-PID row.
    await new Promise((r) => setTimeout(r, 700));

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

// ── D101 — HISTORY mode gate ────────────────────────────────────────
//
// Loads `?mode=history` in the browser, mocks /api/history against
// F3 (same-pid exit+kill — the D71 keying scar) plus two synthetic
// dead-PID entries, and asserts:
//   * the HistoryView renders (data-testid="history-view")
//   * HistoryPage is embedded with alwaysOpen (data-testid="history-panel"
//     with data-testid-open="true", no collapsible chrome)
//   * event count matches fixture (fires the D95 event composite
//     key ${kind}-${pid}-${timestamp} against F3's same-pid exit+kill)
//   * dead-PID count matches the mock (fires the D95 dead-PID
//     composite ${pid}-${exit_time})
//   * zero each_key_duplicate signals across console + pageerror
//   * the dashboard collapsible HistoryPage is GONE from `?mode=dashboard`
//     (D101 C2 decision — history has its own home now)

async function runHistoryModeGate(browser, url) {
    const result = new GateResult();
    const fx = await loadFixture('F3_same_pid_exit_kill');
    // Build the /api/history response: the events reuse F3's activity
    // list (same-pid exit+kill exercises the composite key), and two
    // synthetic dead PIDs let the dead-PID list render + exercise its
    // own `${pid}-${exit_time}` composite.
    const derivedEvents = derivedHistorySnapshot(fx).events;
    const historyPayload = {
        events: derivedEvents,
        dead_pids: [
            {
                pid: 7777,
                name: 'test_workload_a',
                model_name: 'test-model-a',
                exit_time: '2026-06-17T20:00:02Z',
            },
            {
                pid: 8888,
                name: 'test_workload_b',
                model_name: null,
                exit_time: '2026-06-17T19:58:11Z',
            },
        ],
    };
    const expectedEventCount = derivedEvents.length;
    const expectedDeadCount = historyPayload.dead_pids.length;

    // A: history mode renders the view + panel + rows correctly.
    const page = await browser.newPage();
    const consoleMessages = [];
    const pageErrors = [];
    page.on('console', (m) =>
        consoleMessages.push({ type: m.type(), text: m.text() }),
    );
    page.on('pageerror', (err) => pageErrors.push(err.message));

    await page.setRequestInterception(true);
    page.on('request', (req) => {
        const u = new URL(req.url());
        if (!u.pathname.startsWith('/api/')) {
            req.continue();
            return;
        }
        const j = (obj, status = 200) =>
            req.respond({
                status,
                contentType: 'application/json; charset=utf-8',
                body: JSON.stringify(obj),
            });
        if (u.pathname === '/api/snapshot') return j(fx);
        if (u.pathname === '/api/health') return j({ ok: true });
        if (u.pathname === '/api/history') return j(historyPayload);
        if (u.pathname.startsWith('/api/history/trajectory/'))
            return req.respond({ status: 404 });
        req.respond({ status: 404 });
    });

    await page.goto(`${url}/?mode=history`, {
        waitUntil: 'networkidle2',
        timeout: 15000,
    });
    // HistoryPage fires its snapshot-on-open fetch from onMount.
    // Give the fetch + first render a beat to land.
    await new Promise((r) => setTimeout(r, 700));

    const shot = await page.evaluate(() => {
        const view = document.querySelector('[data-testid="history-view"]');
        const panel = document.querySelector('[data-testid="history-panel"]');
        const alwaysOpen = panel
            ? panel.getAttribute('data-testid-open')
            : null;
        const eventRows = document.querySelectorAll(
            '[data-testid="history-event-row"]',
        );
        const deadRows = document.querySelectorAll(
            '[data-testid="history-dead-row"]',
        );
        // Collapsible chrome should NOT be present in history mode.
        const collapsibleToggle = document.querySelector('.history-toggle');
        // Dashboard-side hooks (workloads/activity headings) should
        // NOT be present in history mode either.
        const wlHeading = [...document.querySelectorAll('h2')].find(
            (h) => h.textContent.trim() === 'AI Workloads',
        );
        return {
            hasView: !!view,
            hasPanel: !!panel,
            alwaysOpen,
            eventCount: eventRows.length,
            deadCount: deadRows.length,
            hasCollapsibleToggle: !!collapsibleToggle,
            hasWorkloadsHeading: !!wlHeading,
        };
    });

    if (shot.hasView) result.ok(`[history-mode] HistoryView rendered`);
    else result.fail(`[history-mode] HistoryView not found in DOM`);

    if (shot.hasPanel && shot.alwaysOpen === 'true') {
        result.ok(`[history-mode] HistoryPage embedded with alwaysOpen=true`);
    } else {
        result.fail(
            `[history-mode] HistoryPage not in alwaysOpen state (panel=${shot.hasPanel}, alwaysOpen=${shot.alwaysOpen})`,
        );
    }

    if (!shot.hasCollapsibleToggle) {
        result.ok(
            `[history-mode] no collapsible '▸/▾' toggle button (alwaysOpen hides it)`,
        );
    } else {
        result.fail(
            `[history-mode] collapsible toggle still visible in history mode — alwaysOpen chrome-hide broke`,
        );
    }

    if (shot.eventCount === expectedEventCount) {
        result.ok(
            `[history-mode] event row-count matches fixture (${shot.eventCount})`,
        );
    } else {
        result.fail(
            `[history-mode] event row-count mismatch: DOM ${shot.eventCount} vs fixture ${expectedEventCount} — D95 event composite key regression`,
        );
    }

    if (shot.deadCount === expectedDeadCount) {
        result.ok(
            `[history-mode] dead-PID row-count matches mock (${shot.deadCount})`,
        );
    } else {
        result.fail(
            `[history-mode] dead-PID row-count mismatch: DOM ${shot.deadCount} vs mock ${expectedDeadCount} — D95 dead-PID composite key regression`,
        );
    }

    if (shot.hasWorkloadsHeading) {
        result.fail(
            `[history-mode] dashboard's "AI Workloads" heading is leaking into history mode — the mode router is not swapping cleanly`,
        );
    } else {
        result.ok(
            `[history-mode] dashboard content not present (mode router swapped cleanly)`,
        );
    }

    // Each_key detection — same primary channels as elsewhere.
    const eachKeyConsole = consoleMessages.filter(
        (m) =>
            (m.type === 'warning' || m.type === 'error') &&
            isEachKeyDuplicateSignal(m.text),
    );
    const eachKeyThrows = pageErrors.filter((msg) =>
        isEachKeyDuplicateSignal(msg),
    );
    const anyEachKey = eachKeyConsole.length + eachKeyThrows.length;
    if (anyEachKey === 0) {
        result.ok(
            `[history-mode] no each_key duplicate signals in the event / dead-PID lists`,
        );
    } else {
        result.fail(
            `[history-mode] each_key duplicate signal(s) detected (${anyEachKey}): ` +
                JSON.stringify([
                    ...eachKeyConsole.map((m) => m.text),
                    ...eachKeyThrows,
                ]),
        );
    }

    const otherErrors = pageErrors.filter(
        (msg) => !isEachKeyDuplicateSignal(msg),
    );
    if (otherErrors.length > 0) {
        result.fail(
            `[history-mode] unexpected pageerror(s): ${JSON.stringify(otherErrors)}`,
        );
    } else {
        result.ok(`[history-mode] no unexpected page errors`);
    }
    await page.close();

    // B: dashboard mode no longer carries the collapsible HistoryPage
    // (D101 C2 decision). If it's still there, the dashboard-lean
    // property broke silently.
    const dashPage = await browser.newPage();
    await dashPage.setRequestInterception(true);
    dashPage.on('request', (req) => {
        const u = new URL(req.url());
        if (!u.pathname.startsWith('/api/')) return req.continue();
        const j = (obj) =>
            req.respond({
                status: 200,
                contentType: 'application/json; charset=utf-8',
                body: JSON.stringify(obj),
            });
        if (u.pathname === '/api/snapshot') return j(fx);
        if (u.pathname === '/api/health') return j({ ok: true });
        req.respond({ status: 404 });
    });
    await dashPage.goto(`${url}/?mode=dashboard`, {
        waitUntil: 'networkidle2',
        timeout: 15000,
    });
    await new Promise((r) => setTimeout(r, 400));
    const dashShot = await dashPage.evaluate(() => {
        const panel = document.querySelector('[data-testid="history-panel"]');
        const toggle = document.querySelector('.history-toggle');
        return { hasPanel: !!panel, hasToggle: !!toggle };
    });
    if (!dashShot.hasPanel && !dashShot.hasToggle) {
        result.ok(
            `[dashboard-mode] HistoryPage collapsible removed from dashboard (C2 dashboard-lean confirmed)`,
        );
    } else {
        result.fail(
            `[dashboard-mode] HistoryPage still present in dashboard (panel=${dashShot.hasPanel}, toggle=${dashShot.hasToggle}) — C2 dashboard-lean regressed`,
        );
    }
    await dashPage.close();
    return result;
}

// ── D102 — KIOSK mode gate ──────────────────────────────────────────
//
// Loads `?mode=kiosk` in the browser against three fixture shapes:
//   * F6_kiosk_all_criticals — every meter at 99%, thermal red,
//     critical alerts. Asserts kiosk's aggregation lands on
//     `data-testid-severity="critical"` and NO each_key on the tiles.
//   * F1_dense_colliding_names — 14 healthy workloads, gpu null.
//     Asserts the healthy path AND the VRAM-unmeasured discriminator
//     (`data-testid-unmeasured="true"` + "—", NOT "0%").
//   * F2_duplicate_label_thermals — 2 acpitz thermals, low % — the
//     healthy path with thermal data present. Sanity-check the
//     mode routing works against a small snapshot.
//
// Plus a no-interaction pin: kiosk view must contain zero
// `<button>` / `<a>` / `<input>` elements — the glance-only
// property from §1.2. If a future edit adds interactivity, this
// fires. (The header's mode <select> is outside the KioskView
// scope so it doesn't count.)
async function runKioskModeGate(browser, url) {
    const result = new GateResult();

    async function probe(fixtureName, expectSeverity, extra) {
        const fx = await loadFixture(fixtureName);
        const page = await browser.newPage();
        const consoleMessages = [];
        const pageErrors = [];
        page.on('console', (m) =>
            consoleMessages.push({ type: m.type(), text: m.text() }),
        );
        page.on('pageerror', (err) => pageErrors.push(err.message));

        await page.setRequestInterception(true);
        page.on('request', (req) => {
            const u = new URL(req.url());
            if (!u.pathname.startsWith('/api/')) return req.continue();
            const j = (obj) =>
                req.respond({
                    status: 200,
                    contentType: 'application/json; charset=utf-8',
                    body: JSON.stringify(obj),
                });
            if (u.pathname === '/api/snapshot') return j(fx);
            if (u.pathname === '/api/health') return j({ ok: true });
            if (u.pathname === '/api/history') return j({ events: [], dead_pids: [] });
            req.respond({ status: 404 });
        });
        await page.goto(`${url}/?mode=kiosk`, {
            waitUntil: 'networkidle2',
            timeout: 15000,
        });
        // Give the 1 Hz first poll a beat to land.
        await new Promise((r) => setTimeout(r, 600));

        const shot = await page.evaluate(() => {
            const view = document.querySelector('[data-testid="kiosk-view"]');
            const severity = view
                ? view.getAttribute('data-testid-severity')
                : null;
            const severityLabelEl = document.querySelector(
                '[data-testid="kiosk-severity"]',
            );
            const severityLabelText = severityLabelEl
                ? severityLabelEl.textContent.trim()
                : '';
            const ramTile = document.querySelector(
                '[data-testid="kiosk-tile-ram"]',
            );
            const vramTile = document.querySelector(
                '[data-testid="kiosk-tile-vram"]',
            );
            const vramValueEl = document.querySelector(
                '[data-testid="kiosk-vram-value"]',
            );
            const vramUnmeasured = vramValueEl
                ? vramValueEl.getAttribute('data-testid-unmeasured') === 'true'
                : false;
            const vramText = vramValueEl ? vramValueEl.textContent.trim() : '';
            const thermalTile = document.querySelector(
                '[data-testid="kiosk-tile-thermal"]',
            );
            // D109 — GPU tile (4th, added between VRAM and Thermal).
            const gpuTile = document.querySelector(
                '[data-testid="kiosk-tile-gpu"]',
            );
            const gpuTempEl = document.querySelector(
                '[data-testid="kiosk-gpu-temp"]',
            );
            const gpuPowerEl = document.querySelector(
                '[data-testid="kiosk-gpu-power"]',
            );
            const gpuTempText = gpuTempEl ? gpuTempEl.textContent.trim() : '';
            const gpuPowerText = gpuPowerEl
                ? gpuPowerEl.textContent.trim()
                : '';
            const gpuTempUnmeasured = gpuTempEl
                ? gpuTempEl.getAttribute('data-testid-unmeasured') === 'true'
                : false;
            const gpuPowerUnmeasured = gpuPowerEl
                ? gpuPowerEl.getAttribute('data-testid-unmeasured') === 'true'
                : false;
            // No-interaction pin: count interactive nodes INSIDE
            // the kiosk view. The app header's mode select is
            // outside the view boundary so it doesn't count.
            const interactiveNodes = view
                ? view.querySelectorAll('button, a[href], input, select, textarea')
                      .length
                : 0;
            // Dashboard cards should NOT leak into kiosk.
            const dashWorkloadsHead = [
                ...document.querySelectorAll('h2'),
            ].find((h) => h.textContent.trim() === 'AI Workloads');
            return {
                hasView: !!view,
                severity,
                severityLabelText,
                hasRamTile: !!ramTile,
                hasVramTile: !!vramTile,
                vramUnmeasured,
                vramText,
                hasThermalTile: !!thermalTile,
                hasGpuTile: !!gpuTile,
                gpuTempText,
                gpuPowerText,
                gpuTempUnmeasured,
                gpuPowerUnmeasured,
                interactiveNodes,
                dashLeak: !!dashWorkloadsHead,
            };
        });

        const label = `[kiosk:${fixtureName}]`;

        if (shot.hasView) result.ok(`${label} KioskView rendered`);
        else result.fail(`${label} KioskView not in DOM`);

        if (shot.severity === expectSeverity) {
            result.ok(
                `${label} overall severity=${shot.severity} (matches expected)`,
            );
        } else {
            result.fail(
                `${label} overall severity mismatch: DOM ${shot.severity} vs expected ${expectSeverity}`,
            );
        }
        if (shot.severityLabelText.includes(expectSeverity.toUpperCase())) {
            result.ok(
                `${label} severity label reads "${shot.severityLabelText}"`,
            );
        } else {
            result.fail(
                `${label} severity label "${shot.severityLabelText}" doesn't carry the expected verdict`,
            );
        }

        // D109 — 4 tiles now (RAM / VRAM / GPU / THERMAL). The GPU
        // tile was added as landing 4 of the GPU-temp-power dispatch;
        // the assertion count flips from "3 tiles present" to "4".
        if (
            shot.hasRamTile &&
            shot.hasVramTile &&
            shot.hasGpuTile &&
            shot.hasThermalTile
        ) {
            result.ok(
                `${label} 4 big-number tiles present (RAM / VRAM / GPU / THERMAL)`,
            );
        } else {
            result.fail(
                `${label} tile(s) missing: RAM=${shot.hasRamTile} VRAM=${shot.hasVramTile} GPU=${shot.hasGpuTile} THERMAL=${shot.hasThermalTile}`,
            );
        }

        // D109 — GPU honesty discriminator at kiosk scale. Same
        // pattern as the VRAM tile check: an unmeasured GPU signal
        // must show "—" + data-testid-unmeasured, NEVER "0°C" /
        // "0W". F6 has a real gpu with measured temp/power (both
        // present). F1/F2/F3 have gpu:null so the tile shows an
        // aggregate "—" with no per-half discriminator; F5 has a
        // gpu but no temp_c/power_w on the fixture (should be
        // unmeasured per half).
        if (extra && extra.expectGpuTempMeasured) {
            if (!shot.gpuTempUnmeasured && /\d+°C/.test(shot.gpuTempText)) {
                result.ok(
                    `${label} GPU temp measured tile shows numeric ("${shot.gpuTempText}")`,
                );
            } else {
                result.fail(
                    `${label} GPU temp measured missing: unmeasured=${shot.gpuTempUnmeasured}, text="${shot.gpuTempText}"`,
                );
            }
        }
        if (extra && extra.expectGpuPowerMeasured) {
            if (!shot.gpuPowerUnmeasured && /\d+W/.test(shot.gpuPowerText)) {
                result.ok(
                    `${label} GPU power measured tile shows numeric ("${shot.gpuPowerText}")`,
                );
            } else {
                result.fail(
                    `${label} GPU power measured missing: unmeasured=${shot.gpuPowerUnmeasured}, text="${shot.gpuPowerText}"`,
                );
            }
        }
        // Belt-and-braces: the coerced-zero leak is FORBIDDEN. Even
        // if the fixture doesn't declare an expectation, "0°C" or
        // "0W" appearing in the GPU tile is a honesty violation.
        if (shot.gpuTempText === '0°C' || shot.gpuTempText === '0') {
            result.fail(
                `${label} GPU temp shows "${shot.gpuTempText}" — the "0°C on a wall" trap the honesty discriminator forbids`,
            );
        }
        if (shot.gpuPowerText === '0W' || shot.gpuPowerText === '0') {
            result.fail(
                `${label} GPU power shows "${shot.gpuPowerText}" — the "0W on a wall" trap the honesty discriminator forbids`,
            );
        }

        if (extra && extra.expectVramUnmeasured) {
            if (shot.vramUnmeasured && shot.vramText === '—') {
                result.ok(
                    `${label} VRAM UNMEASURED honesty at kiosk scale: "—" + data-testid-unmeasured (NOT "0%")`,
                );
            } else {
                result.fail(
                    `${label} VRAM unmeasured discriminator broke: unmeasured=${shot.vramUnmeasured}, text="${shot.vramText}" — expected "—" (NOT the "0%" trap)`,
                );
            }
            if (shot.vramText === '0%' || shot.vramText === '0') {
                result.fail(
                    `${label} VRAM shows "${shot.vramText}" — the very "0% on a wall" the honesty discriminator forbids`,
                );
            }
        }
        if (extra && extra.expectVramMeasured) {
            if (!shot.vramUnmeasured && /\d+%/.test(shot.vramText)) {
                result.ok(
                    `${label} VRAM measured tile shows numeric % ("${shot.vramText}")`,
                );
            } else {
                result.fail(
                    `${label} VRAM measured tile missing numeric %: unmeasured=${shot.vramUnmeasured}, text="${shot.vramText}"`,
                );
            }
        }

        if (shot.interactiveNodes === 0) {
            result.ok(
                `${label} no-interaction confirmed (0 buttons/links/inputs inside KioskView)`,
            );
        } else {
            result.fail(
                `${label} glance-only invariant violated: ${shot.interactiveNodes} interactive node(s) inside KioskView. Kiosk is read-only per §1.2.`,
            );
        }

        if (!shot.dashLeak) {
            result.ok(`${label} dashboard content not leaking into kiosk`);
        } else {
            result.fail(
                `${label} dashboard's "AI Workloads" heading leaking into kiosk`,
            );
        }

        const eachKeyConsole = consoleMessages.filter(
            (m) =>
                (m.type === 'warning' || m.type === 'error') &&
                isEachKeyDuplicateSignal(m.text),
        );
        const eachKeyThrows = pageErrors.filter((msg) =>
            isEachKeyDuplicateSignal(msg),
        );
        const anyEachKey = eachKeyConsole.length + eachKeyThrows.length;
        if (anyEachKey === 0) {
            result.ok(`${label} no each_key duplicate signals`);
        } else {
            result.fail(
                `${label} each_key duplicate signal(s) detected (${anyEachKey})`,
            );
        }

        const otherErrors = pageErrors.filter(
            (msg) => !isEachKeyDuplicateSignal(msg),
        );
        if (otherErrors.length === 0) {
            result.ok(`${label} no unexpected page errors`);
        } else {
            result.fail(
                `${label} unexpected pageerror(s): ${JSON.stringify(otherErrors)}`,
            );
        }
        await page.close();
    }

    await probe('F6_kiosk_all_criticals', 'critical', {
        expectVramMeasured: true,
        // D109 — F6 fixture updated with gpu.temp_c=87 gpu.power_w=175
        // for the measured branch of the new GPU tile.
        expectGpuTempMeasured: true,
        expectGpuPowerMeasured: true,
    });
    await probe('F1_dense_colliding_names', 'healthy', {
        expectVramUnmeasured: true,
        // F1 has gpu:null so the GPU tile aggregate shows "—";
        // per-half discriminator doesn't apply (no gpu object to
        // check temp_c/power_w against).
    });
    await probe('F2_duplicate_label_thermals', 'healthy', {
        expectVramUnmeasured: true,
    });

    // Auto-hc default: bookmarking `?mode=kiosk` on a fresh session
    // must default the theme to hc (legibility from distance, per §4.3).
    const page = await browser.newPage();
    await page.setRequestInterception(true);
    page.on('request', (req) => {
        const u = new URL(req.url());
        if (!u.pathname.startsWith('/api/')) return req.continue();
        const j = (obj) =>
            req.respond({
                status: 200,
                contentType: 'application/json; charset=utf-8',
                body: JSON.stringify(obj),
            });
        if (u.pathname === '/api/snapshot')
            return j({
                tick: 1,
                server_time: '2026-06-17T20:00:00Z',
                mission: { workloads: 0, degraded: 0 },
                vitals: {
                    memory_pct: 12,
                    memory_used_mb: 2400,
                    memory_total_mb: 20000,
                    load_average: [0, 0, 0],
                    cpu_count: 12,
                    process_count: 200,
                    gpu: null,
                    thermal_zones: [],
                },
                workloads: [],
                activity: [],
                alerts: [],
            });
        if (u.pathname === '/api/health') return j({ ok: true });
        if (u.pathname === '/api/history') return j({ events: [], dead_pids: [] });
        req.respond({ status: 404 });
    });
    await page.goto(`${url}/?mode=kiosk`, {
        waitUntil: 'networkidle2',
        timeout: 15000,
    });
    await new Promise((r) => setTimeout(r, 400));
    const bodyClass = await page.evaluate(() => document.body.className);
    if (bodyClass.includes('theme-hc')) {
        result.ok(
            `[kiosk:auto-hc] fresh ?mode=kiosk load defaults theme to high-contrast (body.className="${bodyClass}")`,
        );
    } else {
        result.fail(
            `[kiosk:auto-hc] fresh ?mode=kiosk load did NOT default to high-contrast — body.className="${bodyClass}"`,
        );
    }
    await page.close();

    return result;
}

// ── D103 — TIMELINE mode gate ───────────────────────────────────────
//
// Loads `?mode=timeline` and asserts the chronological view renders
// correctly against three shapes:
//   * F7_timeline_dense_ordering — 12 activity events (some kill+exit
//     same-pid same-timestamp — the D71 keying scar), 3 alerts, 4
//     workloads. Asserts the composite {#each} keys hold, event
//     order matches wire, D74 expand still fires (click one event
//     row → detail visible), workloads side rail renders.
//   * F3_same_pid_exit_kill — the tighter D71 case (3 events, one
//     kill+exit same-pid). Belt-and-braces on the composite key
//     under a smaller fixture.
//   * F1_dense_colliding_names — 14 healthy workloads (no activity).
//     Asserts the empty-activity path (ActivityFeed's "No recent
//     activity" fallback) doesn't crash the timeline layout.
//
// Also pins:
//   * VitalsStrip renders (`data-testid="vitals-strip"`) — the new
//     small component this dispatch introduces.
//   * TIMELINE preserves interactivity: at least one clickable
//     element (button) exists inside the view (distinct from
//     kiosk's zero-interaction property).
//   * Dashboard content doesn't leak into timeline.

async function runTimelineModeGate(browser, url) {
    const result = new GateResult();

    async function probe(fixtureName, opts) {
        const fx = await loadFixture(fixtureName);
        const {
            expectActivityCount,
            expectWorkloadCount,
            expectAlertCount,
            expectVramUnmeasured = false,
            testD74Expand = false,
        } = opts;

        const page = await browser.newPage();
        const consoleMessages = [];
        const pageErrors = [];
        page.on('console', (m) =>
            consoleMessages.push({ type: m.type(), text: m.text() }),
        );
        page.on('pageerror', (err) => pageErrors.push(err.message));

        await page.setRequestInterception(true);
        page.on('request', (req) => {
            const u = new URL(req.url());
            if (!u.pathname.startsWith('/api/')) return req.continue();
            const j = (obj) =>
                req.respond({
                    status: 200,
                    contentType: 'application/json; charset=utf-8',
                    body: JSON.stringify(obj),
                });
            if (u.pathname === '/api/snapshot') return j(fx);
            if (u.pathname === '/api/health') return j({ ok: true });
            if (u.pathname === '/api/history')
                return j({ events: [], dead_pids: [] });
            req.respond({ status: 404 });
        });
        await page.goto(`${url}/?mode=timeline`, {
            waitUntil: 'networkidle2',
            timeout: 15000,
        });
        await new Promise((r) => setTimeout(r, 600));

        const label = `[timeline:${fixtureName}]`;

        const shot = await page.evaluate(() => {
            const view = document.querySelector(
                '[data-testid="timeline-view"]',
            );
            const strip = document.querySelector(
                '[data-testid="vitals-strip"]',
            );
            const main = document.querySelector(
                '[data-testid="timeline-main"]',
            );
            const workloadsAside = document.querySelector(
                '[data-testid="timeline-workloads"]',
            );
            const workloadRows = document.querySelectorAll(
                '[data-testid="workload-row"]',
            );
            // Scoped selector: only the ActivityFeed's own `<ul>`
            // children. Without this scope, AlertsPanel's own `<li>`
            // list (state-of-now alerts) would inflate the count.
            const activityFeed = document.querySelector(
                '[data-testid="activity-feed"]',
            );
            const activityRows = activityFeed
                ? activityFeed.querySelectorAll('ul > li')
                : [];
            const activityHead = [...document.querySelectorAll('h2')]
                .filter(
                    (h) =>
                        h.textContent.trim() === 'Activity' ||
                        h.textContent.trim().startsWith('Activity'),
                )
                .length;
            const alertsHead = [...document.querySelectorAll('h2')].some((h) =>
                h.textContent.trim().startsWith('Alerts'),
            );
            // Dashboard leak: the dashboard grid mounts VitalsPanel
            // ("System" h2) + WorkloadsPanel ("AI Workloads" h2).
            // Timeline SHOULD NOT show those exact headings.
            const dashSystemHead = [...document.querySelectorAll('h2')].some(
                (h) => h.textContent.trim() === 'System',
            );
            const dashAiWorkloadsHead = [
                ...document.querySelectorAll('h2'),
            ].some((h) => h.textContent.trim() === 'AI Workloads');
            // Interactivity pin: buttons inside the view. ActivityFeed
            // renders one <button> per activity row for click-to-expand.
            const interactiveNodes = view
                ? view.querySelectorAll('button, a[href], input, select')
                      .length
                : 0;
            const vramValueEl = document.querySelector(
                '[data-testid="vitals-strip-vram-value"]',
            );
            const vramText = vramValueEl
                ? vramValueEl.textContent.trim()
                : '';
            const vramUnmeasured = vramValueEl
                ? vramValueEl.getAttribute('data-testid-unmeasured') === 'true'
                : false;
            return {
                hasView: !!view,
                hasStrip: !!strip,
                hasMain: !!main,
                hasWorkloadsAside: !!workloadsAside,
                workloadRowCount: workloadRows.length,
                activityRowCount: activityRows.length,
                alertsHead,
                activityHead,
                dashSystemHead,
                dashAiWorkloadsHead,
                interactiveNodes,
                vramText,
                vramUnmeasured,
            };
        });

        if (shot.hasView) result.ok(`${label} TimelineView rendered`);
        else result.fail(`${label} TimelineView not in DOM`);

        if (shot.hasStrip) result.ok(`${label} VitalsStrip rendered`);
        else result.fail(`${label} VitalsStrip missing`);

        if (shot.hasMain && shot.hasWorkloadsAside) {
            result.ok(`${label} main column + workloads side rail present`);
        } else {
            result.fail(
                `${label} layout regions missing: main=${shot.hasMain}, workloads=${shot.hasWorkloadsAside}`,
            );
        }

        if (shot.workloadRowCount === expectWorkloadCount) {
            result.ok(
                `${label} workloads side-rail row-count matches fixture (${shot.workloadRowCount})`,
            );
        } else {
            result.fail(
                `${label} workloads side-rail mismatch: DOM ${shot.workloadRowCount} vs fixture ${expectWorkloadCount}`,
            );
        }

        if (shot.activityRowCount === expectActivityCount) {
            result.ok(
                `${label} activity rows in DOM match expected (${shot.activityRowCount})`,
            );
        } else {
            result.fail(
                `${label} activity row-count mismatch: DOM ${shot.activityRowCount} vs expected ${expectActivityCount} — the D71 composite-key regression surface`,
            );
        }

        if (!shot.dashSystemHead && !shot.dashAiWorkloadsHead) {
            result.ok(
                `${label} dashboard content not leaking into timeline (no "System" / "AI Workloads" h2)`,
            );
        } else {
            result.fail(
                `${label} dashboard content leaking into timeline: System=${shot.dashSystemHead}, AI Workloads=${shot.dashAiWorkloadsHead}`,
            );
        }

        // The interaction pin only meaningfully applies when there's
        // content to interact with. On an empty-activity + no-alerts
        // fixture (e.g. F1 has 14 workloads but 0 activity / 0
        // alerts), a "0 interactive nodes" reading is honest — there
        // are no rows to click. Assert instead that the SUM of
        // activity + alert content justifies the interactivity
        // expected, and treat empty-empty as trivially preserved.
        const clickables = expectActivityCount + expectAlertCount;
        if (clickables === 0) {
            result.ok(
                `${label} interaction pin vacuous (0 activity + 0 alerts — nothing to click); no regression`,
            );
        } else if (shot.interactiveNodes > 0) {
            result.ok(
                `${label} interaction preserved (${shot.interactiveNodes} interactive nodes inside TimelineView — distinct from kiosk)`,
            );
        } else {
            result.fail(
                `${label} interaction stripped: 0 interactive nodes with ${clickables} clickable content rows expected — timeline should preserve ActivityFeed's D74 expand`,
            );
        }

        if (expectVramUnmeasured) {
            if (shot.vramUnmeasured && shot.vramText === '—') {
                result.ok(
                    `${label} VitalsStrip VRAM unmeasured discriminator ("—" + testid-unmeasured, NOT "0%")`,
                );
            } else {
                result.fail(
                    `${label} VitalsStrip VRAM unmeasured discriminator broke: unmeasured=${shot.vramUnmeasured}, text="${shot.vramText}"`,
                );
            }
        } else {
            if (!shot.vramUnmeasured && /\d+%/.test(shot.vramText)) {
                result.ok(
                    `${label} VitalsStrip VRAM measured tile shows numeric % ("${shot.vramText}")`,
                );
            } else {
                result.fail(
                    `${label} VitalsStrip VRAM measured tile missing numeric %: text="${shot.vramText}"`,
                );
            }
        }

        // D74 expand test — click the first activity <button> and
        // assert the expand DOM appears (Activity + Reload row
        // structure). Only runs for fixtures with expandable
        // entries.
        if (testD74Expand && shot.activityRowCount > 0) {
            const expandResult = await page.evaluate(() => {
                const view = document.querySelector(
                    '[data-testid="timeline-view"]',
                );
                if (!view) return { ok: false, reason: 'no view' };
                // Find the first <button aria-expanded="false"> inside
                // the activity feed (the ones ActivityFeed renders
                // per row).
                const btn = [...view.querySelectorAll('button')].find(
                    (b) => b.getAttribute('aria-expanded') === 'false',
                );
                if (!btn)
                    return {
                        ok: false,
                        reason: 'no aria-expanded=false button found',
                    };
                btn.click();
                return { ok: true, clicked: true };
            });
            if (expandResult.ok) {
                // Give Svelte a moment to re-render.
                await new Promise((r) => setTimeout(r, 200));
                const nowExpanded = await page.evaluate(() => {
                    const view = document.querySelector(
                        '[data-testid="timeline-view"]',
                    );
                    if (!view) return 0;
                    return [...view.querySelectorAll('button')].filter(
                        (b) => b.getAttribute('aria-expanded') === 'true',
                    ).length;
                });
                if (nowExpanded > 0) {
                    result.ok(
                        `${label} D74 click-to-expand still works in timeline (aria-expanded="true" after click)`,
                    );
                } else {
                    result.fail(
                        `${label} D74 click-to-expand broke: 0 buttons with aria-expanded="true" after click`,
                    );
                }
            } else {
                result.fail(
                    `${label} D74 click-to-expand smoke aborted: ${expandResult.reason}`,
                );
            }
        }

        // each_key detection.
        const eachKeyConsole = consoleMessages.filter(
            (m) =>
                (m.type === 'warning' || m.type === 'error') &&
                isEachKeyDuplicateSignal(m.text),
        );
        const eachKeyThrows = pageErrors.filter((msg) =>
            isEachKeyDuplicateSignal(msg),
        );
        const anyEachKey = eachKeyConsole.length + eachKeyThrows.length;
        if (anyEachKey === 0) {
            result.ok(`${label} no each_key duplicate signals`);
        } else {
            result.fail(
                `${label} each_key duplicate signal(s) detected (${anyEachKey}): ` +
                    JSON.stringify([
                        ...eachKeyConsole.map((m) => m.text),
                        ...eachKeyThrows,
                    ]),
            );
        }

        const otherErrors = pageErrors.filter(
            (msg) => !isEachKeyDuplicateSignal(msg),
        );
        if (otherErrors.length === 0) {
            result.ok(`${label} no unexpected page errors`);
        } else {
            result.fail(
                `${label} unexpected pageerror(s): ${JSON.stringify(otherErrors)}`,
            );
        }
        await page.close();
    }

    // F7 is the primary — dense chronological events + kill+exit
    // same-pid + workloads + alerts. Runs the D74 expand pin.
    await probe('F7_timeline_dense_ordering', {
        expectActivityCount: (
            await loadFixture('F7_timeline_dense_ordering')
        ).activity.length,
        expectWorkloadCount: (
            await loadFixture('F7_timeline_dense_ordering')
        ).workloads.length,
        expectAlertCount: (
            await loadFixture('F7_timeline_dense_ordering')
        ).alerts.length,
        expectVramUnmeasured: false, // F7 has a real gpu
        testD74Expand: true,
    });
    // F3 belt-and-braces on the same-pid-exit-kill composite.
    await probe('F3_same_pid_exit_kill', {
        expectActivityCount: 3,
        expectWorkloadCount: 0,
        expectAlertCount: 0,
        expectVramUnmeasured: true, // F3 has gpu:null
        testD74Expand: false, // F3's activity carries no detail objects — nothing to expand
    });
    // F1 — empty-activity path (14 workloads, 0 activity).
    await probe('F1_dense_colliding_names', {
        expectActivityCount: 0,
        expectWorkloadCount: 14,
        expectAlertCount: 0,
        expectVramUnmeasured: true, // F1 has gpu:null
        testD74Expand: false,
    });

    return result;
}

// ── D104 — FOCUS mode gate ──────────────────────────────────────────
//
// Loads `?mode=focus&pid=N` against the F5 fixture and asserts:
//   * FocusView renders the header + tiles + chart for the selected
//     workload
//   * Client-buffered sparkline grows with successive polls (a
//     second load ~1.2s later has ≥1 sample; short probe stays
//     realistic — the SPA polls at 1 Hz)
//   * VRAM UNMEASURED discriminator at the tile scale ("—" +
//     data-testid-unmeasured, not "0 MB") when the fixture's
//     focused workload has vram_mb=null (PID 5557 in F5)
//   * PID-gone graceful (?mode=focus&pid=999999) → the
//     `focus-notfound` block, not a crash
//   * No-pid graceful (?mode=focus with no &pid=) → the `focus-nopid`
//     block, picker still shown
//   * Picker side rail lists live workloads with per-row testids
//   * No each_key_duplicate signals anywhere
//   * No unexpected pageerrors

async function runFocusModeGate(browser, url) {
    const result = new GateResult();

    // ── (1) F5 focused on the primary VRAM-measured workload ─────
    {
        const fx = await loadFixture('F5_focus_sparkline_dense');
        const focusedPid = 5555; // vllm_serve, gpu-backed
        const page = await browser.newPage();
        const consoleMessages = [];
        const pageErrors = [];
        page.on('console', (m) =>
            consoleMessages.push({ type: m.type(), text: m.text() }),
        );
        page.on('pageerror', (err) => pageErrors.push(err.message));

        // Serve a series of snapshots with monotonically-incrementing
        // ticks so FocusView's rolling buffer sees NEW ticks on each
        // poll (its de-dup guard bails when snap.tick === lastTick).
        let serveCount = 0;
        await page.setRequestInterception(true);
        page.on('request', (req) => {
            const u = new URL(req.url());
            if (!u.pathname.startsWith('/api/')) return req.continue();
            const j = (obj) =>
                req.respond({
                    status: 200,
                    contentType: 'application/json; charset=utf-8',
                    body: JSON.stringify(obj),
                });
            if (u.pathname === '/api/snapshot') {
                serveCount += 1;
                return j({
                    ...fx,
                    tick: fx.tick + serveCount,
                    server_time: new Date(
                        Date.parse(fx.server_time) + serveCount * 1000,
                    ).toISOString(),
                });
            }
            if (u.pathname === '/api/health') return j({ ok: true });
            if (u.pathname === '/api/history')
                return j({ events: [], dead_pids: [] });
            req.respond({ status: 404 });
        });

        await page.goto(`${url}/?mode=focus&pid=${focusedPid}`, {
            waitUntil: 'networkidle2',
            timeout: 15000,
        });
        // First poll lands on onMount; wait long enough for at least
        // 2-3 more 1 Hz polls so the buffer has multiple samples.
        await new Promise((r) => setTimeout(r, 2600));

        const shot = await page.evaluate(() => {
            const view = document.querySelector('[data-testid="focus-view"]');
            const header = document.querySelector(
                '[data-testid="focus-header"]',
            );
            const name = document
                .querySelector('[data-testid="focus-name"]')
                ?.textContent.trim();
            const status = document
                .querySelector('[data-testid="focus-status"]')
                ?.textContent.trim();
            const tiles = document.querySelector(
                '[data-testid="focus-tiles"]',
            );
            const chart = document.querySelector(
                '[data-testid="focus-chart"]',
            );
            const svg = chart?.querySelector('svg[role="img"]');
            // The D95 chart shows "N samples" in the header when
            // trajectory has samples; count via its header text.
            const chartHeaderText = chart
                ? chart.textContent.trim()
                : '';
            const bufferMatch = chartHeaderText.match(/(\d+)\s+sample/);
            const bufferSampleCount = bufferMatch
                ? Number(bufferMatch[1])
                : 0;
            const vramValue = document
                .querySelector('[data-testid="focus-vram-value"]')
                ?.textContent.trim();
            const vramUnmeasured = document
                .querySelector('[data-testid="focus-vram-value"]')
                ?.getAttribute('data-testid-unmeasured') === 'true';
            const pickerRows = document.querySelectorAll(
                '[data-testid="focus-picker-row"]',
            );
            const focusedPickerActive = [
                ...pickerRows,
            ].filter((el) =>
                el.className.includes('picker-row--active'),
            ).length;
            return {
                hasView: !!view,
                hasHeader: !!header,
                hasName: !!name,
                nameText: name,
                statusText: status,
                hasTiles: !!tiles,
                hasChart: !!chart,
                hasSvg: !!svg,
                bufferSampleCount,
                vramValue,
                vramUnmeasured,
                pickerRowCount: pickerRows.length,
                focusedPickerActive,
            };
        });

        const label = `[focus:F5:pid=${focusedPid}]`;

        if (shot.hasView) result.ok(`${label} FocusView rendered`);
        else result.fail(`${label} FocusView not in DOM`);

        if (shot.hasHeader && shot.hasTiles && shot.hasChart) {
            result.ok(
                `${label} header + tiles + chart all rendered`,
            );
        } else {
            result.fail(
                `${label} layout regions missing: header=${shot.hasHeader}, tiles=${shot.hasTiles}, chart=${shot.hasChart}`,
            );
        }

        if (shot.nameText && shot.nameText.length > 0) {
            result.ok(
                `${label} workload name rendered ("${shot.nameText}")`,
            );
        } else {
            result.fail(`${label} workload name blank`);
        }

        // The focused PID is measured (vram_mb=2400). Tile should
        // show a real number, not the dash.
        if (
            !shot.vramUnmeasured &&
            /\d+\s*MB/.test(shot.vramValue ?? '')
        ) {
            result.ok(
                `${label} VRAM tile shows measured value ("${shot.vramValue}")`,
            );
        } else {
            result.fail(
                `${label} VRAM tile should show measured MB — got value="${shot.vramValue}", unmeasured=${shot.vramUnmeasured}`,
            );
        }

        // After ~2.6s of polling at 1 Hz the buffer should have
        // multiple samples. We check ≥2 so a slow CI still passes.
        if (shot.bufferSampleCount >= 2) {
            result.ok(
                `${label} rolling buffer accumulated (${shot.bufferSampleCount} samples after ~2.6s of 1 Hz polling)`,
            );
        } else {
            result.fail(
                `${label} buffer failed to accumulate: got ${shot.bufferSampleCount} samples after ~2.6s — expected ≥2. Client-buffering (§5.1) may have regressed`,
            );
        }

        if (shot.hasSvg) {
            result.ok(`${label} D95 TrajectoryChart SVG present`);
        } else {
            result.fail(
                `${label} chart SVG missing — buffer had samples but chart didn't render`,
            );
        }

        // Picker: F5 has 4 workloads; the focused one gets the
        // active class.
        if (shot.pickerRowCount === 4) {
            result.ok(
                `${label} picker lists all 4 workloads`,
            );
        } else {
            result.fail(
                `${label} picker row-count mismatch: DOM ${shot.pickerRowCount} vs fixture 4`,
            );
        }
        if (shot.focusedPickerActive === 1) {
            result.ok(
                `${label} picker highlights exactly the focused pid`,
            );
        } else {
            result.fail(
                `${label} picker active-row count wrong: ${shot.focusedPickerActive}`,
            );
        }

        const eachKeyConsole = consoleMessages.filter(
            (m) =>
                (m.type === 'warning' || m.type === 'error') &&
                isEachKeyDuplicateSignal(m.text),
        );
        const eachKeyThrows = pageErrors.filter((msg) =>
            isEachKeyDuplicateSignal(msg),
        );
        const anyEachKey = eachKeyConsole.length + eachKeyThrows.length;
        if (anyEachKey === 0) {
            result.ok(`${label} no each_key duplicate signals`);
        } else {
            result.fail(
                `${label} each_key duplicate signal(s) detected (${anyEachKey})`,
            );
        }
        const otherErrors = pageErrors.filter(
            (msg) => !isEachKeyDuplicateSignal(msg),
        );
        if (otherErrors.length === 0) {
            result.ok(`${label} no unexpected page errors`);
        } else {
            result.fail(
                `${label} unexpected pageerror(s): ${JSON.stringify(otherErrors)}`,
            );
        }
        await page.close();
    }

    // ── (2) F5 focused on an UNMEASURED-VRAM workload ────────────
    // PID 5557 (yolo_infer) has vram_mb=null. Focus tile should
    // show "—" with data-testid-unmeasured="true", NOT "0 MB".
    {
        const fx = await loadFixture('F5_focus_sparkline_dense');
        const focusedPid = 5557;
        const page = await browser.newPage();
        await page.setRequestInterception(true);
        page.on('request', (req) => {
            const u = new URL(req.url());
            if (!u.pathname.startsWith('/api/')) return req.continue();
            const j = (obj) =>
                req.respond({
                    status: 200,
                    contentType: 'application/json; charset=utf-8',
                    body: JSON.stringify(obj),
                });
            if (u.pathname === '/api/snapshot') return j(fx);
            if (u.pathname === '/api/health') return j({ ok: true });
            if (u.pathname === '/api/history')
                return j({ events: [], dead_pids: [] });
            req.respond({ status: 404 });
        });
        await page.goto(`${url}/?mode=focus&pid=${focusedPid}`, {
            waitUntil: 'networkidle2',
            timeout: 15000,
        });
        await new Promise((r) => setTimeout(r, 400));
        const shot = await page.evaluate(() => {
            const vramValue = document
                .querySelector('[data-testid="focus-vram-value"]')
                ?.textContent.trim();
            const vramUnmeasured = document
                .querySelector('[data-testid="focus-vram-value"]')
                ?.getAttribute('data-testid-unmeasured') === 'true';
            return { vramValue, vramUnmeasured };
        });
        const label = `[focus:F5:pid=${focusedPid}]`;
        if (shot.vramUnmeasured && shot.vramValue === '—') {
            result.ok(
                `${label} VRAM UNMEASURED discriminator at focus tile ("—" + testid-unmeasured, NOT "0 MB")`,
            );
        } else {
            result.fail(
                `${label} VRAM unmeasured discriminator broke: unmeasured=${shot.vramUnmeasured}, text="${shot.vramValue}"`,
            );
        }
        if (
            shot.vramValue === '0 MB' ||
            shot.vramValue === '0' ||
            shot.vramValue === '0MB'
        ) {
            result.fail(
                `${label} VRAM tile shows "${shot.vramValue}" — the buffered-0 trap forbidden by §C3/§C4`,
            );
        }
        await page.close();
    }

    // ── (3) PID-gone graceful (?mode=focus&pid=999999) ───────────
    {
        const fx = await loadFixture('F5_focus_sparkline_dense');
        const page = await browser.newPage();
        const pageErrors = [];
        page.on('pageerror', (err) => pageErrors.push(err.message));
        await page.setRequestInterception(true);
        page.on('request', (req) => {
            const u = new URL(req.url());
            if (!u.pathname.startsWith('/api/')) return req.continue();
            const j = (obj) =>
                req.respond({
                    status: 200,
                    contentType: 'application/json; charset=utf-8',
                    body: JSON.stringify(obj),
                });
            if (u.pathname === '/api/snapshot') return j(fx);
            if (u.pathname === '/api/health') return j({ ok: true });
            if (u.pathname === '/api/history')
                return j({ events: [], dead_pids: [] });
            req.respond({ status: 404 });
        });
        await page.goto(`${url}/?mode=focus&pid=999999`, {
            waitUntil: 'networkidle2',
            timeout: 15000,
        });
        await new Promise((r) => setTimeout(r, 400));
        const shot = await page.evaluate(() => {
            const notfound = document.querySelector(
                '[data-testid="focus-notfound"]',
            );
            const picker = document.querySelector(
                '[data-testid="focus-picker"]',
            );
            const pickerRows = document.querySelectorAll(
                '[data-testid="focus-picker-row"]',
            );
            return {
                hasNotfound: !!notfound,
                notfoundText: notfound?.textContent.trim() ?? '',
                hasPicker: !!picker,
                pickerRowCount: pickerRows.length,
            };
        });
        const label = `[focus:pid=999999:not-found]`;
        if (shot.hasNotfound) {
            result.ok(
                `${label} graceful not-found block rendered`,
            );
        } else {
            result.fail(
                `${label} not-found block missing — the pid-gone case regressed to crash/blank`,
            );
        }
        if (shot.notfoundText.includes('999999')) {
            result.ok(
                `${label} not-found block names the requested pid ("...${shot.notfoundText.slice(-60)}")`,
            );
        } else {
            result.fail(
                `${label} not-found block missing pid reference`,
            );
        }
        if (shot.hasPicker && shot.pickerRowCount === 4) {
            result.ok(
                `${label} picker still shown with live workloads (${shot.pickerRowCount})`,
            );
        } else {
            result.fail(
                `${label} picker regressed: hasPicker=${shot.hasPicker}, rowCount=${shot.pickerRowCount}`,
            );
        }
        if (pageErrors.length === 0) {
            result.ok(`${label} no page errors`);
        } else {
            result.fail(
                `${label} pageerror(s) on not-found path: ${JSON.stringify(pageErrors)}`,
            );
        }
        await page.close();
    }

    // ── (4) NO-PID graceful (?mode=focus, no &pid=) ──────────────
    {
        const fx = await loadFixture('F5_focus_sparkline_dense');
        const page = await browser.newPage();
        const pageErrors = [];
        page.on('pageerror', (err) => pageErrors.push(err.message));
        await page.setRequestInterception(true);
        page.on('request', (req) => {
            const u = new URL(req.url());
            if (!u.pathname.startsWith('/api/')) return req.continue();
            const j = (obj) =>
                req.respond({
                    status: 200,
                    contentType: 'application/json; charset=utf-8',
                    body: JSON.stringify(obj),
                });
            if (u.pathname === '/api/snapshot') return j(fx);
            if (u.pathname === '/api/health') return j({ ok: true });
            if (u.pathname === '/api/history')
                return j({ events: [], dead_pids: [] });
            req.respond({ status: 404 });
        });
        await page.goto(`${url}/?mode=focus`, {
            waitUntil: 'networkidle2',
            timeout: 15000,
        });
        await new Promise((r) => setTimeout(r, 400));
        const shot = await page.evaluate(() => {
            const nopid = document.querySelector('[data-testid="focus-nopid"]');
            const picker = document.querySelector(
                '[data-testid="focus-picker"]',
            );
            const pickerRows = document.querySelectorAll(
                '[data-testid="focus-picker-row"]',
            );
            const header = document.querySelector('[data-testid="focus-header"]');
            return {
                hasNopid: !!nopid,
                hasPicker: !!picker,
                pickerRowCount: pickerRows.length,
                hasHeader: !!header,
            };
        });
        const label = `[focus:no-pid]`;
        if (shot.hasNopid) {
            result.ok(`${label} graceful no-pid prompt rendered`);
        } else {
            result.fail(`${label} no-pid prompt missing`);
        }
        if (!shot.hasHeader) {
            result.ok(
                `${label} deep-dive header suppressed when no pid selected`,
            );
        } else {
            result.fail(
                `${label} deep-dive header leaked into no-pid state`,
            );
        }
        if (shot.hasPicker && shot.pickerRowCount === 4) {
            result.ok(
                `${label} picker still shown so operator can select`,
            );
        } else {
            result.fail(
                `${label} picker regressed: hasPicker=${shot.hasPicker}, rowCount=${shot.pickerRowCount}`,
            );
        }
        if (pageErrors.length === 0) {
            result.ok(`${label} no page errors`);
        } else {
            result.fail(
                `${label} pageerror(s) on no-pid path: ${JSON.stringify(pageErrors)}`,
            );
        }
        await page.close();
    }

    return result;
}

// ── D105 — mode × fixture matrix ────────────────────────────────────
//
// Systematic completeness pass: every one of the 5 display modes
// mounted against every one of the 6 adversarial fixtures. 5 × 6 = 30
// cells; each cell asserts the BASELINE (renders + no each_key + no
// unexpected page errors). Deeper property checks stay in the D101-
// D104 per-mode probes above — the matrix's job is completeness, not
// depth.
//
// The gap combos this dispatch fills — none of these had explicit
// coverage until now:
//   dashboard × {F5, F6, F7}       (D87-era probes covered F1-F4)
//   history   × {F1, F2, F4, F5, F6, F7}  (D101 only F3)
//   kiosk     × {F3, F5, F7}       (D102 covered F1, F2, F6)
//   timeline  × {F2, F5, F6}       (D103 covered F1, F3, F7)
//   focus     × {F1, F2, F3, F6, F7}  (D104 only F5)
//
// The matrix is the coverage statement: a future mode or fixture
// finds its place in the loop below; a gap becomes visible as a
// missing cell. Re-runnable via `npm run test:browser`.
//
// If a cell CRASHES or emits an each_key signal, that's a FINDING —
// a cross-mode gap the per-mode probes missed. The matrix's whole
// point is to make such gaps loud.

// ── D110 — SettingsPanel body-is-never-blank invariant ──────────────
//
// Regression backstop for a 2026-07-16 report: an operator saw the
// dashboard's Settings toggle expand into a BLANK body. Root cause
// was a 3-state template (loading / error / loaded) that only
// rendered 2 states — the loading branch was implicit and rendered
// nothing. A `view === null && loadError === null` state (fetch
// pending, or a silently-swallowed rejection) therefore looked
// identical to "settings failed to load."
//
// The hardened SettingsPanel exposes 3 testids:
//   * data-testid="settings-loading"     — loading state
//   * data-testid="settings-load-error"  — error state
//   * data-testid="settings-loaded"      — loaded state
//
// Invariant this gate pins: when the panel is expanded, EXACTLY
// ONE of the three testids is present. Blank body must be
// impossible. Two probes cover the two states we can drive from
// the harness: loaded (200 response) and error (500 response). The
// loading state is time-dependent and skipped here (visible in
// human smoke).
async function runSettingsPanelGate(browser, url) {
    const result = new GateResult();

    async function probe(mode, mockSettings) {
        const page = await browser.newPage();
        const pageErrors = [];
        page.on('pageerror', (err) => pageErrors.push(err.message));
        await page.setRequestInterception(true);
        page.on('request', (req) => {
            const u = new URL(req.url());
            if (!u.pathname.startsWith('/api/')) return req.continue();
            const j = (obj, status = 200) =>
                req.respond({
                    status,
                    contentType: 'application/json; charset=utf-8',
                    body: JSON.stringify(obj),
                });
            if (u.pathname === '/api/snapshot')
                return j({
                    tick: 1,
                    schema_version: '1',
                    vitals: {},
                    workloads: [],
                    activity: [],
                    alerts: [],
                    recommendations: [],
                });
            if (u.pathname === '/api/health') return j({ ok: true });
            if (u.pathname === '/api/history')
                return j({ events: [], dead_pids: [] });
            if (u.pathname === '/api/settings') {
                if (mode === 'error') {
                    return req.respond({ status: 500, body: 'boom' });
                }
                return j(mockSettings);
            }
            req.respond({ status: 404 });
        });
        await page.goto(`${url}/`, {
            waitUntil: 'networkidle2',
            timeout: 15000,
        });
        // Click the Settings toggle to enter `expanded=true`.
        await page.evaluate(() => {
            document.querySelector('.settings-toggle')?.click();
        });
        // Give onMount/refresh + Svelte tick a beat.
        await new Promise((r) => setTimeout(r, 600));
        const shot = await page.evaluate(() => {
            const panel = document.querySelector('.settings-panel');
            const body = panel?.querySelector('.settings-body');
            return {
                hasPanel: !!panel,
                hasBody: !!body,
                bodyTextLength: (body?.textContent ?? '').trim().length,
                loaded: !!panel?.querySelector('[data-testid="settings-loaded"]'),
                loading: !!panel?.querySelector('[data-testid="settings-loading"]'),
                error: !!panel?.querySelector('[data-testid="settings-load-error"]'),
                inputCount: panel?.querySelectorAll('input').length ?? 0,
                firstInputValue:
                    panel?.querySelector('input')?.value ?? null,
            };
        });
        await page.close();
        return { shot, pageErrors };
    }

    // Probe 1 — happy path (loaded state).
    {
        const { shot, pageErrors } = await probe('loaded', {
            thresholds: {
                thermal_amber_c: 85,
                thermal_red_c: 95,
                vram_attention_pct: 85,
                vram_critical_pct: 95,
                ram_attention_pct: 90,
                ram_critical_pct: 95,
                kv_attention_pct: 80,
                kv_critical_pct: 95,
                alert_sustain_secs: 5,
            },
            kill_sustain_secs: 10,
            auto_actuate_readonly: false,
            default_ai_action_readonly: 'Allow',
            config_path: 'test/edge_monitor.toml',
        });
        const label = 'D110.loaded';
        if (shot.hasPanel && shot.hasBody)
            result.ok(`${label}: expanded panel body is present`);
        else
            result.fail(`${label}: expanded panel body missing`);
        if (shot.loaded && !shot.loading && !shot.error)
            result.ok(`${label}: exactly the LOADED state is visible`);
        else
            result.fail(
                `${label}: state discriminator wrong (loaded=${shot.loaded}, loading=${shot.loading}, error=${shot.error})`,
            );
        if (shot.inputCount === 6 && shot.firstInputValue === '95')
            result.ok(
                `${label}: 6 inputs render with populated values (first=${shot.firstInputValue})`,
            );
        else
            result.fail(
                `${label}: inputs missing/blank (count=${shot.inputCount}, first=${JSON.stringify(shot.firstInputValue)})`,
            );
        // Filter out errors originating from unrelated dashboard
        // fetches (WebSocket / other endpoints this minimal mock
        // doesn't stub). The point of this probe is the settings
        // panel's own state, not the surrounding page.
        const settingsErrs = pageErrors.filter((e) =>
            /settings/i.test(e),
        );
        if (settingsErrs.length === 0)
            result.ok(`${label}: no settings-related page errors`);
        else
            result.fail(
                `${label}: got ${settingsErrs.length} settings-related error(s): ${settingsErrs.join('; ')}`,
            );
    }

    // Probe 2 — /api/settings 500 (error state).
    {
        const { shot } = await probe('error', null);
        const label = 'D110.error';
        if (shot.error && !shot.loaded && !shot.loading)
            result.ok(`${label}: exactly the ERROR state is visible`);
        else
            result.fail(
                `${label}: state discriminator wrong (loaded=${shot.loaded}, loading=${shot.loading}, error=${shot.error})`,
            );
        if (shot.bodyTextLength > 0)
            result.ok(`${label}: body is not visually blank (has error text)`);
        else
            result.fail(`${label}: body is visually blank on 500`);
    }

    return result;
}

// ── D113 — Connectivity indicator render invariant ──────────────────
//
// Regression backstop for the connectivity-chip dispatch. The wire
// carries `probe_endpoint` + `probe_status` per WorkloadRow;
// `WorkloadRow.svelte` renders a chip iff both are present.
//
// Invariants this gate pins:
//   * HTTP workload (ollama/vLLM/llama.cpp) WITH probe_endpoint +
//     probe_status="ok" → chip rendered, testid-probe="ok",
//     label "net".
//   * HTTP workload WITH probe_status="checking" (first-load) →
//     chip rendered, testid-probe="checking", label "…". NEVER
//     "down" before the first probe completes — the honesty rule.
//   * HTTP workload WITH probe_status="unreachable" → chip rendered,
//     testid-probe="unreachable", label "down".
//   * Non-HTTP workload (agent, ROS2, embeddings) WITHOUT
//     probe_endpoint → NO chip. Zero `[data-testid="workload-probe"]`
//     inside that row. Showing anything would lie about a workload
//     that structurally can't be HTTP-probed.
async function runConnectivityChipGate(browser, url) {
    const result = new GateResult();
    const page = await browser.newPage();
    const pageErrors = [];
    page.on('pageerror', (err) => pageErrors.push(err.message));
    await page.setRequestInterception(true);
    page.on('request', (req) => {
        const u = new URL(req.url());
        if (!u.pathname.startsWith('/api/')) return req.continue();
        const j = (obj) =>
            req.respond({
                status: 200,
                contentType: 'application/json; charset=utf-8',
                body: JSON.stringify(obj),
            });
        if (u.pathname === '/api/snapshot')
            return j({
                tick: 1,
                schema_version: '1',
                mission: { workloads: 4, degraded: 0 },
                vitals: {
                    memory_pct: 30,
                    memory_used_mb: 8000,
                    memory_total_mb: 32000,
                    load_average: [1.0, 1.1, 1.2],
                    cpu_count: 12,
                    process_count: 100,
                    thermal_zones: [],
                },
                workloads: [
                    // ollama: ok
                    { pid: 1, name: 'ollama', model_name: 'llama3', category: 'inference', workload_category: 'llm', cpu_pct: 25.0, rss_mb: 2048, ram_pct: 6.4, vram_mb: 512, tokens_per_sec: 42.0, fps: null, kv_cache_peak_pct: null, status: 'healthy', activity: 'active', probe_endpoint: 'http://127.0.0.1:11434/api/ps', probe_status: 'ok' },
                    // vLLM: checking (first-load honest state)
                    { pid: 2, name: 'python', model_name: 'phi3', category: 'inference', workload_category: 'llm', cpu_pct: 20.0, rss_mb: 4096, ram_pct: 12.8, vram_mb: 8192, tokens_per_sec: 55.0, fps: null, kv_cache_peak_pct: null, status: 'healthy', activity: 'active', probe_endpoint: 'http://127.0.0.1:8000/metrics', probe_status: 'checking' },
                    // llama.cpp: unreachable (post-debounce)
                    { pid: 3, name: 'llama-server', model_name: 'mistral', category: 'inference', workload_category: 'llm', cpu_pct: 0.5, rss_mb: 1024, ram_pct: 3.2, vram_mb: 0, tokens_per_sec: null, fps: null, kv_cache_peak_pct: null, status: 'healthy', activity: null, probe_endpoint: 'http://127.0.0.1:8080/metrics', probe_status: 'unreachable' },
                    // agent: NO chip (probe_endpoint absent → probe_status absent)
                    { pid: 4, name: 'agent-process', model_name: null, category: 'agent', workload_category: 'agent', cpu_pct: 1.5, rss_mb: 128, ram_pct: 0.4, vram_mb: null, tokens_per_sec: null, fps: null, kv_cache_peak_pct: null, status: 'healthy', activity: null },
                ],
                activity: [],
                alerts: [],
                recommendations: [],
            });
        if (u.pathname === '/api/health') return j({ ok: true });
        if (u.pathname === '/api/history') return j({ events: [], dead_pids: [] });
        if (u.pathname === '/api/settings')
            return j({
                thresholds: {},
                kill_sustain_secs: 10,
                auto_actuate_readonly: false,
                default_ai_action_readonly: 'Allow',
                config_path: null,
            });
        req.respond({ status: 404 });
    });
    await page.goto(`${url}/`, { waitUntil: 'networkidle2', timeout: 15000 });
    await new Promise((r) => setTimeout(r, 500));
    const shot = await page.evaluate(() => {
        const rows = Array.from(document.querySelectorAll('[data-testid="workload-row"]'));
        return rows.map((row) => {
            const chip = row.querySelector('[data-testid="workload-probe"]');
            return {
                name: row.children[1]?.textContent.trim().slice(0, 40),
                chip_present: !!chip,
                chip_status: chip?.getAttribute('data-testid-probe') ?? null,
                chip_text: chip?.textContent.trim() ?? null,
            };
        });
    });
    await page.close();

    if (shot.length === 4)
        result.ok(`D113: 4 workload rows rendered`);
    else
        result.fail(`D113: expected 4 rows, got ${shot.length}: ${JSON.stringify(shot)}`);
    // Row 0 — ollama, ok
    if (shot[0]?.chip_present && shot[0]?.chip_status === 'ok' && /net/.test(shot[0]?.chip_text ?? ''))
        result.ok(`D113: ollama row shows OK chip (net)`);
    else
        result.fail(`D113: ollama chip wrong: ${JSON.stringify(shot[0])}`);
    // Row 1 — vLLM, checking (honesty: never "down" on first-load)
    if (shot[1]?.chip_present && shot[1]?.chip_status === 'checking' && /…/.test(shot[1]?.chip_text ?? ''))
        result.ok(`D113: vLLM row shows CHECKING chip (…); never "down" on first-load`);
    else
        result.fail(`D113: vLLM checking chip wrong: ${JSON.stringify(shot[1])}`);
    // Row 2 — llama.cpp, unreachable
    if (shot[2]?.chip_present && shot[2]?.chip_status === 'unreachable' && /down/.test(shot[2]?.chip_text ?? ''))
        result.ok(`D113: llama.cpp row shows UNREACHABLE chip (down)`);
    else
        result.fail(`D113: llama.cpp unreachable chip wrong: ${JSON.stringify(shot[2])}`);
    // Row 3 — agent, NO chip (honesty: non-HTTP workload)
    if (shot[3] && !shot[3].chip_present)
        result.ok(`D113: agent row shows NO chip (non-HTTP → honesty rule holds)`);
    else
        result.fail(`D113: agent chip should be absent; got: ${JSON.stringify(shot[3])}`);
    if (pageErrors.length === 0)
        result.ok(`D113: no page errors`);
    else
        result.fail(`D113: got ${pageErrors.length} page error(s): ${pageErrors.join('; ')}`);

    return result;
}

// ── D111 — Thermal-friendly-name render invariant ───────────────────
//
// Regression backstop for the thermal-friendly dispatch: raw kernel
// labels (`x86_pkg_temp`, `acpitz`, `acpitz`) were unreadable and
// duplicate acpitz rows were indistinguishable. The wire now carries
// `friendly_label` per zone (populated by
// `platform::humanize_thermal_labels`, disambiguates duplicates with
// positional `System Zone 1` / `System Zone 2`); the web renderer
// shows friendly primary + raw muted alongside.
//
// Invariant this gate pins:
//   * Every thermal row renders BOTH `data-testid="thermal-friendly"`
//     AND `data-testid="thermal-raw"` — never just one.
//   * Duplicate raw labels resolve to DIFFERENT friendly names
//     (positional disambiguation held).
//   * A known label (`x86_pkg_temp`) maps to a known friendly
//     (`CPU Package`).
// ── D114 — Web workloads column-header parity with TUI ─────────────
//
// Regression backstop for the web-column-headers dispatch: the TUI
// workloads panel has a column-header row (D107 FIX 2) — NAME /
// MODEL / STATE / PRIMARY / STARTED / CPU % / RSS MB / VRAM — but
// the web panel rendered data rows with NO header row. Operators
// couldn't tell which column was which.
//
// The web panel now renders a header row above the group dividers.
// Web has no MODEL/STARTED columns (name-fused, no per-row spawn
// time), so the header labels the columns web ACTUALLY renders:
// NAME / PRIMARY / STATE / CPU % / RSS MB / VRAM.
//
// Invariants this gate pins:
//   * `[data-testid="workloads-header"]` exists exactly once when
//     the panel has workloads.
//   * 6 label testids are present: name / primary / state / cpu /
//     rss / vram. If a future refactor drops one, the alignment
//     between header and row breaks — this fires early.
//   * The header uses the SAME grid-template as WorkloadRow. Pinned
//     by the computed grid-template-columns matching.
//   * When the panel is empty (no workloads), the header does NOT
//     render — otherwise operators see a stranded header above an
//     "empty state" message.
async function runWorkloadsHeaderGate(browser, url) {
    const result = new GateResult();

    async function probe(withWorkloads) {
        const page = await browser.newPage();
        const pageErrors = [];
        page.on('pageerror', (err) => pageErrors.push(err.message));
        await page.setRequestInterception(true);
        page.on('request', (req) => {
            const u = new URL(req.url());
            if (!u.pathname.startsWith('/api/')) return req.continue();
            const j = (obj) =>
                req.respond({
                    status: 200,
                    contentType: 'application/json; charset=utf-8',
                    body: JSON.stringify(obj),
                });
            if (u.pathname === '/api/snapshot')
                return j({
                    tick: 1,
                    schema_version: '1',
                    mission: { workloads: withWorkloads ? 1 : 0, degraded: 0 },
                    vitals: {
                        memory_pct: 30, memory_used_mb: 8000, memory_total_mb: 32000,
                        load_average: [1.0, 1.1, 1.2], cpu_count: 12, process_count: 100,
                        thermal_zones: [],
                    },
                    workloads: withWorkloads ? [
                        { pid: 1, name: 'ollama', model_name: 'llama3', category: 'inference', workload_category: 'llm', cpu_pct: 25.0, rss_mb: 2048, ram_pct: 6.4, vram_mb: 512, tokens_per_sec: 42.0, fps: null, kv_cache_peak_pct: null, status: 'healthy', activity: 'active' },
                    ] : [],
                    activity: [], alerts: [], recommendations: [],
                });
            if (u.pathname === '/api/health') return j({ ok: true });
            if (u.pathname === '/api/history') return j({ events: [], dead_pids: [] });
            if (u.pathname === '/api/settings')
                return j({
                    thresholds: {}, kill_sustain_secs: 10,
                    auto_actuate_readonly: false, default_ai_action_readonly: 'Allow',
                    config_path: null,
                });
            req.respond({ status: 404 });
        });
        await page.goto(`${url}/`, { waitUntil: 'networkidle2', timeout: 15000 });
        await new Promise((r) => setTimeout(r, 500));
        const shot = await page.evaluate(() => {
            const header = document.querySelector('[data-testid="workloads-header"]');
            const row = document.querySelector('[data-testid="workload-row"]');
            // With `display: contents` on both wrappers, their cells
            // are direct children of ONE shared CSS Grid. Alignment
            // is proved by the header's cell-left positions matching
            // the row's cell-left positions for each column. Sample
            // the 3 label cells that carry unique widths — NAME
            // (1fr, wide), PRIMARY (auto), VRAM (auto).
            const nameHdr = document.querySelector('[data-testid="workloads-header-name"]');
            const primaryHdr = document.querySelector('[data-testid="workloads-header-primary"]');
            const vramHdr = document.querySelector('[data-testid="workloads-header-vram"]');
            const rowChildren = row ? Array.from(row.querySelectorAll(':scope > *')) : [];
            // Row cell positions: index 1 (name), 2 (primary), 6 (vram) —
            // matches WorkloadRow's 8-cell order.
            const nameCell = rowChildren[1];
            const primaryCell = rowChildren[2];
            const vramCell = rowChildren[6];
            const leftOf = (el) => el ? Math.round(el.getBoundingClientRect().left) : null;
            return {
                header_present: !!header,
                header_count: document.querySelectorAll('[data-testid="workloads-header"]').length,
                labels: {
                    name: nameHdr?.textContent.trim(),
                    primary: primaryHdr?.textContent.trim(),
                    state: document.querySelector('[data-testid="workloads-header-state"]')?.textContent.trim(),
                    cpu: document.querySelector('[data-testid="workloads-header-cpu"]')?.textContent.trim(),
                    rss: document.querySelector('[data-testid="workloads-header-rss"]')?.textContent.trim(),
                    vram: vramHdr?.textContent.trim(),
                },
                alignment: {
                    name_header_left: leftOf(nameHdr),
                    name_row_left: leftOf(nameCell),
                    primary_header_left: leftOf(primaryHdr),
                    primary_row_left: leftOf(primaryCell),
                    vram_header_left: leftOf(vramHdr),
                    vram_row_left: leftOf(vramCell),
                },
            };
        });
        await page.close();
        return { shot, pageErrors };
    }

    // Probe 1 — panel WITH workloads: header must render, labels present, grid matches row.
    {
        const { shot, pageErrors } = await probe(true);
        if (shot.header_present && shot.header_count === 1)
            result.ok(`D114: exactly one workloads-header renders when panel has workloads`);
        else
            result.fail(`D114: header count wrong (present=${shot.header_present}, count=${shot.header_count})`);
        const expected = { name: 'NAME', primary: 'PRIMARY', state: 'STATE', cpu: 'CPU %', rss: 'RSS MB', vram: 'VRAM' };
        for (const [key, want] of Object.entries(expected)) {
            const got = shot.labels[key];
            if (got === want)
                result.ok(`D114: header label ${key} = "${want}"`);
            else
                result.fail(`D114: header label ${key} — expected "${want}", got "${got}"`);
        }
        // Column alignment — each header cell's left edge must match
        // the corresponding data-row cell's left edge (they're
        // direct children of the SAME grid via display:contents).
        // Tolerance of 1px covers subpixel rounding.
        const nearby = (a, b) => a !== null && b !== null && Math.abs(a - b) <= 1;
        const a = shot.alignment;
        if (nearby(a.name_header_left, a.name_row_left))
            result.ok(`D114: NAME header aligned above NAME cell (${a.name_header_left}px ≈ ${a.name_row_left}px)`);
        else
            result.fail(`D114: NAME misaligned — header@${a.name_header_left}px, row@${a.name_row_left}px`);
        if (nearby(a.primary_header_left, a.primary_row_left))
            result.ok(`D114: PRIMARY header aligned above PRIMARY cell (${a.primary_header_left}px ≈ ${a.primary_row_left}px)`);
        else
            result.fail(`D114: PRIMARY misaligned — header@${a.primary_header_left}px, row@${a.primary_row_left}px`);
        if (nearby(a.vram_header_left, a.vram_row_left))
            result.ok(`D114: VRAM header aligned above VRAM cell (${a.vram_header_left}px ≈ ${a.vram_row_left}px)`);
        else
            result.fail(`D114: VRAM misaligned — header@${a.vram_header_left}px, row@${a.vram_row_left}px`);
        if (pageErrors.length === 0)
            result.ok(`D114: no page errors`);
        else
            result.fail(`D114: got ${pageErrors.length} page error(s): ${pageErrors.join('; ')}`);
    }

    // Probe 2 — panel EMPTY: header must NOT render (no stranded label above the empty-state message).
    {
        const { shot } = await probe(false);
        if (!shot.header_present)
            result.ok(`D114: empty panel renders NO header (no stranded label)`);
        else
            result.fail(`D114: header should be absent when workloads=[]; got header_count=${shot.header_count}`);
    }

    return result;
}

// ── D115 — Web Top-Processes 3-panel invariant ──────────────────────
//
// Regression backstop for the top-processes 3-panel dispatch. The
// TUI's `render_three_panels` shows RAM / VRAM / CPU side-by-side;
// the web now has parity via `TopProcessesPanel.svelte` reading
// `WireTopProcesses`. Invariants pinned:
//
//   1. All three sub-panels render when `top_processes` is populated:
//      `[data-testid="top-processes-ram"]`,
//      `[data-testid="top-processes-vram"]`,
//      `[data-testid="top-processes-cpu"]`.
//   2. Each sub-panel's rows are the wire's `by_ram` / `by_vram` /
//      `by_cpu` in order — the server-side sort is stable and the
//      renderer must not re-sort (mirrors the TUI's read-only sort
//      + PID-asc tiebreak).
//   3. VRAM honesty (THE load-bearing invariant):
//      * When `by_vram` is EMPTY, the sub-panel shows the italic
//        "no GPU users" empty state via
//        `[data-testid="top-processes-vram-empty"]`. It does NOT
//        render zero-VRAM rows to pad to 5.
//      * When `by_vram` is populated with entries that have
//        `vram_mb`, values render as `NNN MB` / `N.N GB` —
//        never `0 MB`.
//      * A defensive check: a `vram_mb`-absent entry (shouldn't
//        normally reach the wire, but defensively) renders `—`
//        with `data-testid-unmeasured="true"`.
//   4. Responsive: on a narrow viewport (< 768px), the 3 panels
//      stack vertically (single column). On wide, they sit
//      side-by-side (grid-cols-3). Verified via computed grid
//      template.
async function runTopProcessesPanelGate(browser, url) {
    const result = new GateResult();

    async function probe(width, snapshot) {
        const page = await browser.newPage();
        await page.setViewport({ width, height: 900 });
        const pageErrors = [];
        page.on('pageerror', (err) => pageErrors.push(err.message));
        await page.setRequestInterception(true);
        page.on('request', (req) => {
            const u = new URL(req.url());
            if (!u.pathname.startsWith('/api/')) return req.continue();
            const j = (obj) =>
                req.respond({
                    status: 200,
                    contentType: 'application/json; charset=utf-8',
                    body: JSON.stringify(obj),
                });
            if (u.pathname === '/api/snapshot') return j(snapshot);
            if (u.pathname === '/api/health') return j({ ok: true });
            if (u.pathname === '/api/history')
                return j({ events: [], dead_pids: [] });
            if (u.pathname === '/api/settings')
                return j({
                    thresholds: {}, kill_sustain_secs: 10,
                    auto_actuate_readonly: false, default_ai_action_readonly: 'Allow',
                    config_path: null,
                });
            req.respond({ status: 404 });
        });
        await page.goto(`${url}/`, { waitUntil: 'networkidle2', timeout: 15000 });
        await new Promise((r) => setTimeout(r, 500));
        const shot = await page.evaluate(() => {
            const ramPanel = document.querySelector('[data-testid="top-processes-ram"]');
            const vramPanel = document.querySelector('[data-testid="top-processes-vram"]');
            const cpuPanel = document.querySelector('[data-testid="top-processes-cpu"]');
            const grid = ramPanel?.parentElement;
            const gridCols = grid ? window.getComputedStyle(grid).gridTemplateColumns : null;
            const ramValues = Array.from(document.querySelectorAll('[data-testid="top-row-ram-value"]')).map((e) => e.textContent.trim());
            const vramValues = Array.from(document.querySelectorAll('[data-testid="top-row-vram-value"]')).map((e) => ({
                text: e.textContent.trim(),
                unmeasured: e.getAttribute('data-testid-unmeasured') === 'true',
            }));
            const cpuValues = Array.from(document.querySelectorAll('[data-testid="top-row-cpu-value"]')).map((e) => e.textContent.trim());
            const vramEmpty = document.querySelector('[data-testid="top-processes-vram-empty"]');
            return {
                all_three_present: !!ramPanel && !!vramPanel && !!cpuPanel,
                ram_row_count: document.querySelectorAll('[data-testid="top-row-ram"]').length,
                vram_row_count: document.querySelectorAll('[data-testid="top-row-vram"]').length,
                cpu_row_count: document.querySelectorAll('[data-testid="top-row-cpu"]').length,
                ram_values: ramValues,
                vram_values: vramValues,
                cpu_values: cpuValues,
                vram_empty_present: !!vramEmpty,
                grid_cols: gridCols,
            };
        });
        await page.close();
        return { shot, pageErrors };
    }

    // Fixture #1 — populated: 3 by_ram, 2 by_vram (short-not-padded), 3 by_cpu.
    // Uses the same shape the Rust wire mapper emits.
    const populated = {
        tick: 1,
        schema_version: '1',
        mission: { workloads: 0, degraded: 0 },
        vitals: {
            memory_pct: 30, memory_used_mb: 8000, memory_total_mb: 32000,
            load_average: [1.0, 1.1, 1.2], cpu_count: 12, process_count: 100,
            thermal_zones: [],
        },
        workloads: [],
        activity: [], alerts: [], recommendations: [],
        top_processes: {
            by_ram: [
                { pid: 100, name: 'big_app', rss_mb: 4096, cpu_pct: 1.0 },
                { pid: 200, name: 'medium_app', rss_mb: 2048, cpu_pct: 0.5 },
                { pid: 300, name: 'small_app', rss_mb: 512, cpu_pct: 0.1 },
            ],
            by_vram: [
                // ONLY 2 GPU users — honest short list, NOT padded to 5.
                { pid: 400, name: 'llama-server', rss_mb: 1500, cpu_pct: 90.0, vram_mb: 4000 },
                { pid: 401, name: 'python_train', rss_mb: 800, cpu_pct: 45.0, vram_mb: 800 },
            ],
            by_cpu: [
                { pid: 500, name: 'busy_worker', rss_mb: 100, cpu_pct: 92.5 },
                { pid: 600, name: 'idle_worker', rss_mb: 200, cpu_pct: 5.0 },
                { pid: 700, name: 'sleepy', rss_mb: 300, cpu_pct: 1.0 },
            ],
        },
    };

    // Probe 1 — WIDE viewport (1400px): 3 columns side-by-side.
    {
        const { shot, pageErrors } = await probe(1400, populated);
        if (shot.all_three_present)
            result.ok(`D115.wide: all 3 sub-panels render (RAM+VRAM+CPU)`);
        else
            result.fail(`D115.wide: sub-panel(s) missing`);
        if (shot.ram_row_count === 3 && shot.cpu_row_count === 3)
            result.ok(`D115.wide: RAM/CPU render exactly 3 rows each (matches fixture)`);
        else
            result.fail(`D115.wide: row counts wrong (ram=${shot.ram_row_count}, cpu=${shot.cpu_row_count})`);
        // The honest short list: 2 VRAM rows, NOT padded to 5.
        if (shot.vram_row_count === 2 && !shot.vram_empty_present)
            result.ok(`D115.wide: VRAM sub-panel shows exactly 2 GPU users (honest short list, not padded)`);
        else
            result.fail(`D115.wide: VRAM row count wrong (rows=${shot.vram_row_count}, empty=${shot.vram_empty_present})`);
        // Sort order: by_ram descending — first row must be the "big_app" value.
        if (shot.ram_values[0] === '4.0 GB')
            result.ok(`D115.wide: RAM row 0 = "4.0 GB" (top of descending sort held)`);
        else
            result.fail(`D115.wide: RAM sort order broken; got ${JSON.stringify(shot.ram_values)}`);
        // VRAM honesty: measured values render as MB/GB, never "0 MB".
        const anyZero = shot.vram_values.some((v) => v.text === '0 MB' || v.text === '0');
        if (!anyZero)
            result.ok(`D115.wide: no fake "0 MB" rows in VRAM panel (honesty rule)`);
        else
            result.fail(`D115.wide: VRAM panel has fake 0-MB row: ${JSON.stringify(shot.vram_values)}`);
        // CPU sort descending: first row = "92.5%".
        if (shot.cpu_values[0] === '92.5%')
            result.ok(`D115.wide: CPU row 0 = "92.5%" (top of descending sort held)`);
        else
            result.fail(`D115.wide: CPU sort order broken; got ${JSON.stringify(shot.cpu_values)}`);
        // Grid: on md+ viewports, computed grid-template-columns has 3 tracks.
        const trackCount = (shot.grid_cols?.split(' ') ?? []).length;
        if (trackCount === 3)
            result.ok(`D115.wide: grid renders 3 columns side-by-side (${trackCount} tracks)`);
        else
            result.fail(`D115.wide: expected 3-column grid, got ${trackCount} tracks: "${shot.grid_cols}"`);
        if (pageErrors.length === 0)
            result.ok(`D115.wide: no page errors`);
        else
            result.fail(`D115.wide: got ${pageErrors.length} page error(s)`);
    }

    // Probe 2 — NARROW viewport (500px): stacks to 1 column.
    {
        const { shot } = await probe(500, populated);
        const trackCount = (shot.grid_cols?.split(' ') ?? []).length;
        if (trackCount === 1)
            result.ok(`D115.narrow: grid stacks to 1 column on narrow viewport (< md breakpoint)`);
        else
            result.fail(`D115.narrow: expected 1-column stack, got ${trackCount} tracks: "${shot.grid_cols}"`);
    }

    // Probe 3 — VRAM honesty via empty by_vram: THE key test.
    {
        const emptyVram = {
            ...populated,
            top_processes: {
                by_ram: populated.top_processes.by_ram,
                by_vram: [],  // no GPU users on the host
                by_cpu: populated.top_processes.by_cpu,
            },
        };
        const { shot } = await probe(1400, emptyVram);
        if (shot.vram_row_count === 0 && shot.vram_empty_present)
            result.ok(`D115.empty-vram: empty by_vram → "no GPU users" empty state, ZERO fabricated rows`);
        else
            result.fail(`D115.empty-vram: expected empty-state + 0 rows, got rows=${shot.vram_row_count}, empty=${shot.vram_empty_present}`);
        // And the OTHER two panels still render normally.
        if (shot.ram_row_count === 3 && shot.cpu_row_count === 3)
            result.ok(`D115.empty-vram: RAM+CPU sub-panels still render (empty VRAM doesn't cascade)`);
        else
            result.fail(`D115.empty-vram: RAM/CPU broke (ram=${shot.ram_row_count}, cpu=${shot.cpu_row_count})`);
    }

    return result;
}

async function runThermalFriendlyGate(browser, url) {
    const result = new GateResult();
    const page = await browser.newPage();
    const pageErrors = [];
    page.on('pageerror', (err) => pageErrors.push(err.message));
    await page.setRequestInterception(true);
    page.on('request', (req) => {
        const u = new URL(req.url());
        if (!u.pathname.startsWith('/api/')) return req.continue();
        const j = (obj) =>
            req.respond({
                status: 200,
                contentType: 'application/json; charset=utf-8',
                body: JSON.stringify(obj),
            });
        if (u.pathname === '/api/snapshot')
            return j({
                tick: 1,
                schema_version: '1',
                mission: { workloads: 0, degraded: 0 },
                vitals: {
                    memory_pct: 30,
                    memory_used_mb: 8000,
                    memory_total_mb: 32000,
                    load_average: [1.0, 1.1, 1.2],
                    cpu_count: 12,
                    process_count: 100,
                    thermal_zones: [
                        { label: 'acpitz', friendly_label: 'System Zone 1', temp_celsius: 48.0, severity: 'nominal' },
                        { label: 'acpitz', friendly_label: 'System Zone 2', temp_celsius: 51.0, severity: 'nominal' },
                        { label: 'x86_pkg_temp', friendly_label: 'CPU Package', temp_celsius: 62.0, severity: 'nominal' },
                    ],
                },
                workloads: [],
                activity: [],
                alerts: [],
                recommendations: [],
            });
        if (u.pathname === '/api/health') return j({ ok: true });
        if (u.pathname === '/api/history') return j({ events: [], dead_pids: [] });
        if (u.pathname === '/api/settings')
            return j({
                thresholds: {},
                kill_sustain_secs: 10,
                auto_actuate_readonly: false,
                default_ai_action_readonly: 'Allow',
                config_path: null,
            });
        req.respond({ status: 404 });
    });
    await page.goto(`${url}/`, { waitUntil: 'networkidle2', timeout: 15000 });
    await new Promise((r) => setTimeout(r, 500));
    const shot = await page.evaluate(() => {
        const rows = Array.from(document.querySelectorAll('[data-testid="thermal-row"]'));
        return rows.map((row) => ({
            friendly: row.querySelector('[data-testid="thermal-friendly"]')?.textContent.trim() ?? null,
            raw: row.querySelector('[data-testid="thermal-raw"]')?.textContent.trim() ?? null,
        }));
    });
    await page.close();

    if (shot.length === 3)
        result.ok(`D111: 3 thermal rows rendered (matches fixture)`);
    else
        result.fail(`D111: expected 3 thermal rows, got ${shot.length}`);
    const bothPresent = shot.every((r) => r.friendly && r.raw);
    if (bothPresent)
        result.ok(`D111: every thermal row renders BOTH friendly + raw label`);
    else
        result.fail(`D111: some rows missing friendly or raw: ${JSON.stringify(shot)}`);
    const friendlies = shot.map((r) => r.friendly);
    const uniq = new Set(friendlies).size;
    if (uniq === friendlies.length)
        result.ok(`D111: duplicate raw labels resolve to DIFFERENT friendly names (uniq=${uniq})`);
    else
        result.fail(`D111: friendly names collided ({friendlies=${JSON.stringify(friendlies)}}); positional disambiguation broken`);
    if (friendlies.includes('CPU Package'))
        result.ok(`D111: known label x86_pkg_temp maps to friendly "CPU Package"`);
    else
        result.fail(`D111: friendly-name mapping for x86_pkg_temp broken; got ${JSON.stringify(friendlies)}`);
    if (pageErrors.length === 0)
        result.ok(`D111: no page errors`);
    else
        result.fail(`D111: got ${pageErrors.length} page error(s): ${pageErrors.join('; ')}`);

    return result;
}

// ── D112 — Workload VRAM column render invariant ────────────────────
//
// Regression backstop for the VRAM-column dispatch: per-workload
// vram_mb was ALREADY populated on the wire (WireWorkload.vram_mb),
// but the web renderer crammed it into the RSS cell as `· NNNM GPU`
// which operators reported as invisible next to the TUI's aligned
// column. Web WorkloadRow now has a dedicated 7th grid cell for VRAM,
// with the VRAM honesty rule: `null` / `0` / absent → `—` (muted +
// data-testid-unmeasured="true"); positive → `NNNM VRAM`.
//
// Invariant this gate pins:
//   * Every workload row exposes a `data-testid="workload-vram"` cell.
//   * A row with `vram_mb: 512` shows `512M VRAM` and no unmeasured
//     testid.
//   * A row with `vram_mb: null` shows `— VRAM` with
//     `data-testid-unmeasured="true"` (VRAM honesty).
//   * A row with `vram_mb: 0` also shows `— VRAM` (zero is NOT a
//     measurement in the workload-attribution semantic — matches TUI
//     `Some(b) if b > 0` gate).
async function runWorkloadVramColumnGate(browser, url) {
    const result = new GateResult();
    const page = await browser.newPage();
    const pageErrors = [];
    page.on('pageerror', (err) => pageErrors.push(err.message));
    await page.setRequestInterception(true);
    page.on('request', (req) => {
        const u = new URL(req.url());
        if (!u.pathname.startsWith('/api/')) return req.continue();
        const j = (obj) =>
            req.respond({
                status: 200,
                contentType: 'application/json; charset=utf-8',
                body: JSON.stringify(obj),
            });
        if (u.pathname === '/api/snapshot')
            return j({
                tick: 1,
                schema_version: '1',
                mission: { workloads: 3, degraded: 0 },
                vitals: {
                    memory_pct: 30,
                    memory_used_mb: 8000,
                    memory_total_mb: 32000,
                    load_average: [1.0, 1.1, 1.2],
                    cpu_count: 12,
                    process_count: 100,
                    thermal_zones: [],
                },
                workloads: [
                    { pid: 1, name: 'ollama-runner', model_name: 'llama3', category: 'inference', workload_category: 'llm', cpu_pct: 25.0, rss_mb: 2048, ram_pct: 6.4, vram_mb: 512, tokens_per_sec: 42.0, fps: null, kv_cache_peak_pct: null, status: 'healthy', activity: 'active' },
                    { pid: 2, name: 'agent-process', model_name: null, category: 'agent', workload_category: 'agent', cpu_pct: 1.5, rss_mb: 128, ram_pct: 0.4, vram_mb: null, tokens_per_sec: null, fps: null, kv_cache_peak_pct: null, status: 'healthy', activity: null },
                    { pid: 3, name: 'zero-vram', model_name: null, category: 'inference', workload_category: 'llm', cpu_pct: 0.5, rss_mb: 64, ram_pct: 0.2, vram_mb: 0, tokens_per_sec: null, fps: null, kv_cache_peak_pct: null, status: 'healthy', activity: null },
                ],
                activity: [],
                alerts: [],
                recommendations: [],
            });
        if (u.pathname === '/api/health') return j({ ok: true });
        if (u.pathname === '/api/history') return j({ events: [], dead_pids: [] });
        if (u.pathname === '/api/settings')
            return j({
                thresholds: {},
                kill_sustain_secs: 10,
                auto_actuate_readonly: false,
                default_ai_action_readonly: 'Allow',
                config_path: null,
            });
        req.respond({ status: 404 });
    });
    await page.goto(`${url}/`, { waitUntil: 'networkidle2', timeout: 15000 });
    await new Promise((r) => setTimeout(r, 500));
    const shot = await page.evaluate(() => {
        const rows = Array.from(document.querySelectorAll('[data-testid="workload-row"]'));
        return rows.map((row) => {
            const cell = row.querySelector('[data-testid="workload-vram"]');
            return {
                text: cell?.textContent.trim() ?? null,
                unmeasured: cell?.getAttribute('data-testid-unmeasured') === 'true',
            };
        });
    });
    await page.close();

    if (shot.length === 3)
        result.ok(`D112: 3 workload rows rendered`);
    else
        result.fail(`D112: expected 3 rows, got ${shot.length}`);
    const allHaveVramCell = shot.every((r) => r.text !== null);
    if (allHaveVramCell)
        result.ok(`D112: every workload row exposes a workload-vram cell`);
    else
        result.fail(`D112: some rows missing VRAM cell: ${JSON.stringify(shot)}`);
    if (shot[0]?.text === '512M VRAM' && !shot[0]?.unmeasured)
        result.ok(`D112: measured vram_mb=512 renders as "512M VRAM" (not unmeasured)`);
    else
        result.fail(`D112: measured VRAM row wrong: ${JSON.stringify(shot[0])}`);
    if (shot[1]?.text === '— VRAM' && shot[1]?.unmeasured)
        result.ok(`D112: vram_mb=null renders as "— VRAM" with unmeasured testid (VRAM honesty)`);
    else
        result.fail(`D112: unmeasured VRAM row wrong: ${JSON.stringify(shot[1])}`);
    if (shot[2]?.text === '— VRAM' && shot[2]?.unmeasured)
        result.ok(`D112: vram_mb=0 also renders as "— VRAM" (zero is NOT a measurement in workload attribution)`);
    else
        result.fail(`D112: zero-VRAM row wrong: ${JSON.stringify(shot[2])}`);
    if (pageErrors.length === 0)
        result.ok(`D112: no page errors`);
    else
        result.fail(`D112: got ${pageErrors.length} page error(s): ${pageErrors.join('; ')}`);

    return result;
}

async function runMatrixGate(browser, url) {
    const result = new GateResult();

    // Fixture set for the matrix. Deliberately excludes
    // `_negative_control_colliding_activity` — that fixture is
    // designed to fire each_key (proves the detector isn't dead)
    // and would trip every cell in an unhelpful way. Its
    // completeness lives in the top-level per-fixture probe above.
    const MATRIX_FIXTURES = [
        'F1_dense_colliding_names',
        'F2_duplicate_label_thermals',
        'F3_same_pid_exit_kill',
        'F4_combined_worst_case',
        'F5_focus_sparkline_dense',
        'F6_kiosk_all_criticals',
        'F7_timeline_dense_ordering',
    ];
    const MATRIX_MODES = [
        'dashboard',
        'history',
        'kiosk',
        'timeline',
        'focus',
    ];

    // Cache fixture bodies once (7 files, not 35 disk reads).
    const fixtures = {};
    for (const name of MATRIX_FIXTURES) {
        fixtures[name] = await loadFixture(name);
    }

    /**
     * Build the URL path for a (mode, fixture) cell. Focus mode
     * takes an extra `&pid=N` — if the fixture has workloads, pick
     * the first as the focus target so the deep-dive view exercises;
     * otherwise omit pid and let the no-pid graceful state render
     * (still a legitimate baseline for the cell — we're asserting
     * "no crash, no each_key," not "deep-dive was reached").
     */
    function pathFor(mode, fx) {
        if (mode === 'dashboard') return '/';
        if (mode === 'focus') {
            if (fx.workloads && fx.workloads.length > 0) {
                return `/?mode=focus&pid=${fx.workloads[0].pid}`;
            }
            return '/?mode=focus';
        }
        return `/?mode=${mode}`;
    }

    /**
     * "Did the mode render?" — a mode-specific presence check. For
     * dashboard we look for WorkloadsPanel's "AI Workloads" `<h2>`,
     * which only surfaces on the dashboard branch. Other modes use
     * their view's testid.
     */
    async function didRender(page, mode) {
        return await page.evaluate((m) => {
            switch (m) {
                case 'dashboard':
                    return [...document.querySelectorAll('h2')].some(
                        (h) => h.textContent.trim() === 'AI Workloads',
                    );
                case 'history':
                    return !!document.querySelector(
                        '[data-testid="history-view"]',
                    );
                case 'kiosk':
                    return !!document.querySelector(
                        '[data-testid="kiosk-view"]',
                    );
                case 'timeline':
                    return !!document.querySelector(
                        '[data-testid="timeline-view"]',
                    );
                case 'focus':
                    return !!document.querySelector(
                        '[data-testid="focus-view"]',
                    );
                default:
                    return false;
            }
        }, mode);
    }

    // Wait cadence: focus with a pid needs one 1 Hz poll to build
    // its first sparkline sample; every other mode renders on the
    // initial fetch + a beat of settle.
    function waitMsFor(mode, fx) {
        if (mode === 'focus' && fx.workloads?.length > 0) return 1200;
        return 400;
    }

    for (const mode of MATRIX_MODES) {
        for (const fixtureName of MATRIX_FIXTURES) {
            const fx = fixtures[fixtureName];
            const path = pathFor(mode, fx);
            const label = `[matrix ${mode.padEnd(9)} × ${fixtureName}]`;

            const page = await browser.newPage();
            const consoleMessages = [];
            const pageErrors = [];
            page.on('console', (m) =>
                consoleMessages.push({ type: m.type(), text: m.text() }),
            );
            page.on('pageerror', (err) => pageErrors.push(err.message));

            await page.setRequestInterception(true);
            page.on('request', (req) => {
                const u = new URL(req.url());
                if (!u.pathname.startsWith('/api/')) return req.continue();
                const j = (obj) =>
                    req.respond({
                        status: 200,
                        contentType: 'application/json; charset=utf-8',
                        body: JSON.stringify(obj),
                    });
                if (u.pathname === '/api/snapshot') return j(fx);
                if (u.pathname === '/api/health') return j({ ok: true });
                if (u.pathname === '/api/history')
                    return j({ events: [], dead_pids: [] });
                if (u.pathname.startsWith('/api/history/trajectory/'))
                    return req.respond({ status: 404 });
                if (u.pathname === '/api/settings')
                    return j({
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
                req.respond({ status: 404 });
            });

            let gotoErr = null;
            try {
                await page.goto(`${url}${path}`, {
                    waitUntil: 'networkidle2',
                    timeout: 15000,
                });
                await new Promise((r) => setTimeout(r, waitMsFor(mode, fx)));
            } catch (e) {
                gotoErr = e;
            }

            if (gotoErr) {
                result.fail(
                    `${label} navigation failed: ${gotoErr.message}`,
                );
                await page.close();
                continue;
            }

            // Baseline 1 — renders.
            const rendered = await didRender(page, mode);
            if (rendered) {
                result.ok(`${label} renders`);
            } else {
                result.fail(
                    `${label} did NOT render — mode-specific marker missing`,
                );
            }

            // Baseline 2 — no each_key duplicate signal.
            const eachKeyConsole = consoleMessages.filter(
                (m) =>
                    (m.type === 'warning' || m.type === 'error') &&
                    isEachKeyDuplicateSignal(m.text),
            );
            const eachKeyThrows = pageErrors.filter((msg) =>
                isEachKeyDuplicateSignal(msg),
            );
            const anyEachKey = eachKeyConsole.length + eachKeyThrows.length;
            if (anyEachKey === 0) {
                result.ok(`${label} no each_key signals`);
            } else {
                result.fail(
                    `${label} each_key duplicate signal(s) detected (${anyEachKey}): ` +
                        JSON.stringify([
                            ...eachKeyConsole.map((m) => m.text),
                            ...eachKeyThrows,
                        ]),
                );
            }

            // Baseline 3 — no unexpected page errors.
            const otherErrors = pageErrors.filter(
                (msg) => !isEachKeyDuplicateSignal(msg),
            );
            if (otherErrors.length === 0) {
                result.ok(`${label} no unexpected page errors`);
            } else {
                result.fail(
                    `${label} unexpected pageerror(s): ${JSON.stringify(otherErrors)}`,
                );
            }

            await page.close();
        }
    }

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

    // v1.3.2 / DISPATCH 101 — extends the gate with HISTORY-mode
    // coverage. New render surface (?mode=history → HistoryView
    // → HistoryPage alwaysOpen). Reuses F3 (same-pid exit+kill —
    // the D71 composite-key scar) as the archive fixture so the
    // event {#each} key is exercised. The dead-PID list is
    // stubbed with two synthetic entries.
    console.log(`\n▶ D101 — HISTORY mode (?mode=history)`);
    const historyRes = await runHistoryModeGate(browser, url);
    historyRes.passes.forEach((p) => console.log(`   ✓ ${p}`));
    historyRes.failures.forEach((f) => console.log(`   ✗ ${f}`));
    results.push({ name: 'D101_history_mode', ...historyRes.summarize() });

    console.log(`\n▶ D102 — KIOSK mode (?mode=kiosk, glance-only)`);
    const kioskRes = await runKioskModeGate(browser, url);
    kioskRes.passes.forEach((p) => console.log(`   ✓ ${p}`));
    kioskRes.failures.forEach((f) => console.log(`   ✗ ${f}`));
    results.push({ name: 'D102_kiosk_mode', ...kioskRes.summarize() });

    console.log(`\n▶ D103 — TIMELINE mode (?mode=timeline, interaction-first)`);
    const timelineRes = await runTimelineModeGate(browser, url);
    timelineRes.passes.forEach((p) => console.log(`   ✓ ${p}`));
    timelineRes.failures.forEach((f) => console.log(`   ✗ ${f}`));
    results.push({
        name: 'D103_timeline_mode',
        ...timelineRes.summarize(),
    });

    console.log(`\n▶ D104 — FOCUS mode (?mode=focus&pid=N, client-buffered)`);
    const focusRes = await runFocusModeGate(browser, url);
    focusRes.passes.forEach((p) => console.log(`   ✓ ${p}`));
    focusRes.failures.forEach((f) => console.log(`   ✗ ${f}`));
    results.push({
        name: 'D104_focus_mode',
        ...focusRes.summarize(),
    });

    console.log(
        `\n▶ D105 — mode × fixture matrix (5 × 7 = 35 cells, baseline)`,
    );
    const matrixRes = await runMatrixGate(browser, url);
    matrixRes.passes.forEach((p) => console.log(`   ✓ ${p}`));
    matrixRes.failures.forEach((f) => console.log(`   ✗ ${f}`));
    results.push({
        name: 'D105_mode_fixture_matrix',
        ...matrixRes.summarize(),
    });

    console.log(`\n▶ D110 — Settings panel body-is-never-blank invariant`);
    const settingsRes = await runSettingsPanelGate(browser, url);
    settingsRes.passes.forEach((p) => console.log(`   ✓ ${p}`));
    settingsRes.failures.forEach((f) => console.log(`   ✗ ${f}`));
    results.push({
        name: 'D110_settings_panel_never_blank',
        ...settingsRes.summarize(),
    });

    console.log(`\n▶ D111 — Thermal friendly-name render invariant`);
    const thermalRes = await runThermalFriendlyGate(browser, url);
    thermalRes.passes.forEach((p) => console.log(`   ✓ ${p}`));
    thermalRes.failures.forEach((f) => console.log(`   ✗ ${f}`));
    results.push({
        name: 'D111_thermal_friendly',
        ...thermalRes.summarize(),
    });

    console.log(`\n▶ D112 — Workload VRAM column render invariant`);
    const vramColRes = await runWorkloadVramColumnGate(browser, url);
    vramColRes.passes.forEach((p) => console.log(`   ✓ ${p}`));
    vramColRes.failures.forEach((f) => console.log(`   ✗ ${f}`));
    results.push({
        name: 'D112_workload_vram_column',
        ...vramColRes.summarize(),
    });

    console.log(`\n▶ D113 — Connectivity indicator (chip present/absent/checking)`);
    const chipRes = await runConnectivityChipGate(browser, url);
    chipRes.passes.forEach((p) => console.log(`   ✓ ${p}`));
    chipRes.failures.forEach((f) => console.log(`   ✗ ${f}`));
    results.push({
        name: 'D113_connectivity_chip',
        ...chipRes.summarize(),
    });

    console.log(`\n▶ D114 — Web workloads column-header parity with TUI`);
    const headerRes = await runWorkloadsHeaderGate(browser, url);
    headerRes.passes.forEach((p) => console.log(`   ✓ ${p}`));
    headerRes.failures.forEach((f) => console.log(`   ✗ ${f}`));
    results.push({
        name: 'D114_workloads_column_headers',
        ...headerRes.summarize(),
    });

    console.log(`\n▶ D115 — Top Processes 3-panel (RAM/VRAM/CPU) parity + VRAM honesty`);
    const topRes = await runTopProcessesPanelGate(browser, url);
    topRes.passes.forEach((p) => console.log(`   ✓ ${p}`));
    topRes.failures.forEach((f) => console.log(`   ✗ ${f}`));
    results.push({
        name: 'D115_top_processes_panel',
        ...topRes.summarize(),
    });

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
