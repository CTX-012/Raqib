#!/usr/bin/env bash
# verify-edge-monitor.sh
#
# Independent verification script for edge_monitor on Linux.
# Mirrors Verify-EdgeMonitor.ps1 phase-for-phase so reports across
# Linux and Windows are directly comparable.
#
# USAGE:
#   ./verify-edge-monitor.sh                        # full audit
#   ./verify-edge-monitor.sh --skip-slow            # skip Ollama ground-truth test
#   ./verify-edge-monitor.sh --project-root /path   # explicit project path
#
# OUTPUT:
#   ./audit_results/REPORT.md            human-readable report
#   ./audit_results/evidence/*.txt       raw command output for every check
#
# RULES:
#   - Every PASS must be backed by captured output. No "looks correct."
#   - Every FAIL must have a file path, line number, or command output.
#   - SKIP is acceptable. ASSUMED is not.

set -u  # don't use -e; we want to keep going on individual failures

# ---------------------------------------------------------------------------
# Globals
# ---------------------------------------------------------------------------

PROJECT_ROOT="$(pwd)"
SKIP_SLOW=0
OUTPUT_DIR="audit_results"
EVIDENCE_DIR=""
START_TIME=$(date +%s)

# Findings collected as parallel arrays
declare -a F_ID F_DESC F_STATUS F_SEV F_NOTES F_TIME

# Colors (only if stdout is a TTY)
if [[ -t 1 ]]; then
    C_RED=$'\033[0;31m'
    C_GREEN=$'\033[0;32m'
    C_YELLOW=$'\033[0;33m'
    C_GRAY=$'\033[0;90m'
    C_CYAN=$'\033[0;36m'
    C_RESET=$'\033[0m'
else
    C_RED=""; C_GREEN=""; C_YELLOW=""; C_GRAY=""; C_CYAN=""; C_RESET=""
fi

# ---------------------------------------------------------------------------
# Argument parsing
# ---------------------------------------------------------------------------

while [[ $# -gt 0 ]]; do
    case "$1" in
        --skip-slow)        SKIP_SLOW=1; shift ;;
        --project-root)     PROJECT_ROOT="$2"; shift 2 ;;
        --output-dir)       OUTPUT_DIR="$2"; shift 2 ;;
        -h|--help)
            grep '^#' "$0" | head -25 | sed 's/^# \?//'
            exit 0 ;;
        *)
            echo "Unknown argument: $1" >&2
            exit 1 ;;
    esac
done

# ---------------------------------------------------------------------------
# Helpers
# ---------------------------------------------------------------------------

initialize_audit() {
    cd "$PROJECT_ROOT" || { echo "Cannot cd to $PROJECT_ROOT" >&2; exit 1; }
    if [[ ! -f Cargo.toml ]]; then
        echo "No Cargo.toml at $PROJECT_ROOT — wrong directory?" >&2
        exit 1
    fi

    EVIDENCE_DIR="$OUTPUT_DIR/evidence"
    mkdir -p "$EVIDENCE_DIR"

    echo "${C_CYAN}Audit starting in: $PROJECT_ROOT${C_RESET}"
    echo "${C_CYAN}Evidence dir: $EVIDENCE_DIR${C_RESET}"
    echo
}

save_evidence() {
    local name="$1"
    local content="$2"
    local path="$EVIDENCE_DIR/${name}.txt"
    printf '%s\n' "$content" > "$path"
    echo "$path"
}

add_finding() {
    # add_finding ID DESC STATUS [SEVERITY] [NOTES]
    local id="$1" desc="$2" status="$3" sev="${4:-}" notes="${5:-}"
    F_ID+=("$id")
    F_DESC+=("$desc")
    F_STATUS+=("$status")
    F_SEV+=("$sev")
    F_NOTES+=("$notes")
    F_TIME+=("$(date +%H:%M:%S)")

    local color
    case "$status" in
        PASS) color="$C_GREEN" ;;
        FAIL) color="$C_RED" ;;
        WARN) color="$C_YELLOW" ;;
        SKIP) color="$C_GRAY" ;;
        *)    color="$C_RESET" ;;
    esac
    printf '%s[%s] %s — %s%s\n' "$color" "$status" "$id" "$desc" "$C_RESET"
    [[ -n "$notes" ]] && printf '%s         %s%s\n' "$C_GRAY" "$notes" "$C_RESET"
}

# ---------------------------------------------------------------------------
# Phase A: Build & basic invariants
# ---------------------------------------------------------------------------

test_build_and_clippy() {
    echo
    echo "${C_CYAN}=== Phase A: Build & static checks ===${C_RESET}"

    # A.1 — does it build at all?
    local out
    out=$(cargo build --release 2>&1)
    local rc=$?
    save_evidence "A1_build" "$out" >/dev/null
    if [[ $rc -eq 0 ]]; then
        add_finding 'A.1' 'cargo build --release succeeds' 'PASS'
    else
        add_finding 'A.1' 'cargo build --release succeeds' 'FAIL' 'S0' \
            "Build failed; nothing else can be verified. Stop and fix."
        return 1
    fi

    # A.2 — clippy clean
    out=$(cargo clippy --all-targets -- -D warnings 2>&1)
    rc=$?
    save_evidence "A2_clippy" "$out" >/dev/null
    if [[ $rc -eq 0 ]]; then
        add_finding 'A.2' 'cargo clippy clean (no warnings)' 'PASS'
    else
        add_finding 'A.2' 'cargo clippy clean' 'FAIL' 'S2' \
            "See A2_clippy.txt for diagnostics."
    fi

    # A.3 — fmt clean
    out=$(cargo fmt --all -- --check 2>&1)
    rc=$?
    save_evidence "A3_fmt" "$out" >/dev/null
    if [[ $rc -eq 0 ]]; then
        add_finding 'A.3' 'cargo fmt clean' 'PASS'
    else
        add_finding 'A.3' 'cargo fmt clean' 'WARN' '' "Formatting drift; not a blocker."
    fi

    # A.4 — binary exists & --version
    EXE="$PROJECT_ROOT/target/release/edge_monitor"
    if [[ ! -x "$EXE" ]]; then
        # try finding it
        EXE=$(find "$PROJECT_ROOT/target" -type f -name 'edge_monitor' -executable 2>/dev/null | head -1)
    fi

    if [[ -n "${EXE:-}" && -x "$EXE" ]]; then
        out=$("$EXE" --version 2>&1)
        rc=$?
        save_evidence "A4_version" "$out" >/dev/null
        if [[ $rc -eq 0 ]] && echo "$out" | grep -qE '[0-9]+\.[0-9]+\.[0-9]+'; then
            add_finding 'A.4' '--version runs and returns semver' 'PASS' '' "$(echo "$out" | head -1)"
        else
            add_finding 'A.4' '--version' 'FAIL' 'S2' "$out"
        fi
    else
        add_finding 'A.4' 'binary exists at expected path' 'FAIL' 'S0' \
            "edge_monitor not found in target/release/"
        return 1
    fi
    return 0
}

# ---------------------------------------------------------------------------
# Phase B: Test suite — counts, names, empty-binary check
# ---------------------------------------------------------------------------

test_suite() {
    echo
    echo "${C_CYAN}=== Phase B: Test suite verification ===${C_RESET}"

    # B.1 — full test run
    local out
    out=$(cargo test --all 2>&1)
    save_evidence "B1_test_run" "$out" >/dev/null

    # Parse all "test result: ok. N passed; M failed" lines
    local total_passed=0 total_failed=0
    local empty_bins=0 populated_bins=0 binary_count=0
    while IFS= read -r line; do
        if [[ "$line" =~ test\ result:\ ok\.\ ([0-9]+)\ passed\;\ ([0-9]+)\ failed ]]; then
            local p="${BASH_REMATCH[1]}"
            local f="${BASH_REMATCH[2]}"
            total_passed=$((total_passed + p))
            total_failed=$((total_failed + f))
            binary_count=$((binary_count + 1))
            if [[ $p -eq 0 && $f -eq 0 ]]; then
                empty_bins=$((empty_bins + 1))
            else
                populated_bins=$((populated_bins + 1))
            fi
        fi
    done <<< "$out"

    if [[ $binary_count -eq 0 ]]; then
        add_finding 'B.1' 'cargo test runs and reports results' 'FAIL' 'S0' \
            "Could not parse any test result lines. Test infrastructure broken."
        return
    fi

    if [[ $total_failed -gt 0 ]]; then
        add_finding 'B.1' "cargo test all-pass" 'FAIL' 'S0' \
            "$total_passed passed, $total_failed FAILED. See B1_test_run.txt."
    else
        add_finding 'B.1' "cargo test runs all green" 'PASS' '' \
            "$total_passed tests across $binary_count binaries"
    fi

    # B.2 — empty binary check
    if [[ $empty_bins -gt 0 ]]; then
        add_finding 'B.2' 'no empty test binaries' 'WARN' '' \
            "$empty_bins binaries have ZERO tests (scaffolds without content). $populated_bins have real tests."
    else
        add_finding 'B.2' 'no empty test binaries' 'PASS'
    fi

    # B.3 — list every test by name
    out=$(cargo test --all -- --list 2>&1)
    save_evidence "B3_test_list" "$out" >/dev/null
    local test_count
    test_count=$(echo "$out" | grep -cE ': test$' || true)
    add_finding 'B.3' 'test count via --list' 'PASS' '' \
        "$test_count test functions enumerated."

    # B.4 — average per-test runtime sanity
    local total_time=0
    local times
    times=$(echo "$out" | grep -oE 'finished in [0-9.]+s' | grep -oE '[0-9.]+' || true)
    while IFS= read -r t; do
        [[ -z "$t" ]] && continue
        # bash-portable float add via awk
        total_time=$(awk -v a="$total_time" -v b="$t" 'BEGIN{print a+b}')
    done <<< "$times"

    if [[ $total_passed -gt 0 ]]; then
        local avg_ms
        avg_ms=$(awk -v t="$total_time" -v n="$total_passed" 'BEGIN{printf "%.2f", (t/n)*1000}')
        local note="avg per-test runtime: ${avg_ms} ms ($total_passed tests in ${total_time}s)"
        # Compare avg_ms < 0.5
        if awk -v x="$avg_ms" 'BEGIN{exit !(x < 0.5)}'; then
            add_finding 'B.4' 'tests appear to exercise real behavior' 'WARN' '' \
                "$note — under 0.5ms/test suggests assertions on constants. Spot-check test bodies."
        else
            add_finding 'B.4' 'tests appear to exercise real behavior' 'PASS' '' "$note"
        fi
    fi
}

# ---------------------------------------------------------------------------
# Phase C: Production code quality
# ---------------------------------------------------------------------------

test_production_quality() {
    echo
    echo "${C_CYAN}=== Phase C: Production code quality ===${C_RESET}"

    # C.1 — no .unwrap() outside tests
    local unwrap_hits
    unwrap_hits=$(grep -rn '\.unwrap()' --include='*.rs' src crates 2>/dev/null \
        | grep -v '/tests/' \
        | grep -v '_test\.rs' \
        | grep -v '// ok: unwrap' \
        | grep -v '#\[cfg(test)\]' \
        || true)
    save_evidence "C1_unwraps" "$unwrap_hits" >/dev/null

    if [[ -z "$unwrap_hits" ]]; then
        add_finding 'C.1' 'no .unwrap() in production code' 'PASS'
    else
        local count
        count=$(echo "$unwrap_hits" | wc -l)
        add_finding 'C.1' 'no .unwrap() in production code' 'WARN' 'S3' \
            "$count unwrap() calls found. See C1_unwraps.txt for file:line list."
    fi

    # C.2 — no .expect() outside tests (the Linux audit's actual finding)
    local expect_hits
    expect_hits=$(grep -rn '\.expect(' --include='*.rs' src crates 2>/dev/null \
        | grep -v '/tests/' \
        | grep -v '_test\.rs' \
        | grep -v '// ok: expect' \
        | grep -v '#\[cfg(test)\]' \
        || true)
    save_evidence "C2_expects" "$expect_hits" >/dev/null

    if [[ -z "$expect_hits" ]]; then
        add_finding 'C.2' 'no .expect() in production code' 'PASS'
    else
        local count
        count=$(echo "$expect_hits" | wc -l)
        add_finding 'C.2' 'no .expect() in production code' 'WARN' 'S3' \
            "$count expect() calls. Linux audit flagged audit.rs:76 and stdout_parser.rs:40,49,62. Document or refactor."
    fi

    # C.3 — TODO/FIXME markers
    local todos
    todos=$(grep -rn -E 'TODO|FIXME|XXX|HACK' --include='*.rs' src crates 2>/dev/null || true)
    save_evidence "C3_todos" "$todos" >/dev/null
    local todo_count
    todo_count=$(echo "$todos" | grep -c . || echo 0)

    if [[ $todo_count -eq 0 ]]; then
        add_finding 'C.3' 'no TODO/FIXME markers' 'PASS'
    elif [[ $todo_count -lt 10 ]]; then
        add_finding 'C.3' 'no TODO/FIXME markers' 'WARN' '' \
            "$todo_count markers — small enough to clean up."
    else
        add_finding 'C.3' 'no TODO/FIXME markers' 'WARN' 'S4' \
            "$todo_count markers in production code."
    fi
}

# ---------------------------------------------------------------------------
# Phase D: Identity unification (mostly Windows-specific, but check anyway)
# ---------------------------------------------------------------------------

test_identity_unification() {
    echo
    echo "${C_CYAN}=== Phase D: Identity / structural drift ===${C_RESET}"

    # D.1 — old branding leakage (mostly relevant for Windows merging into shared core)
    local vatch
    vatch=$(grep -rli 'VATCH' src crates 2>/dev/null || true)
    save_evidence "D1_vatch" "$vatch" >/dev/null
    if [[ -z "$vatch" ]]; then
        add_finding 'D.1' 'no VATCH branding leakage' 'PASS'
    else
        add_finding 'D.1' 'no VATCH branding leakage' 'WARN' 'S3' \
            "VATCH branding found in $(echo "$vatch" | wc -l) files. If merging Linux+Windows codebases, clean up."
    fi

    # D.2 — old vocabulary
    local oldvocab
    oldvocab=$(grep -rli 'VISION & AI INFERENCE REGISTRY' src crates 2>/dev/null || true)
    save_evidence "D2_old_vocab" "$oldvocab" >/dev/null
    if [[ -z "$oldvocab" ]]; then
        add_finding 'D.2' 'no old Windows vocabulary' 'PASS'
    else
        add_finding 'D.2' 'no old Windows vocabulary' 'WARN' '' \
            "Old strings found in $(echo "$oldvocab" | wc -l) files."
    fi

    # D.3 — config schema check
    local jsons
    jsons=$(find . -name 'config.json' -not -path '*/target/*' -not -path '*/node_modules/*' 2>/dev/null || true)
    save_evidence "D3_config_json" "$jsons" >/dev/null
    if [[ -z "$jsons" ]] && [[ -f "edge_monitor.toml.example" ]]; then
        add_finding 'D.3' 'config is TOML (no stray config.json)' 'PASS'
    elif [[ -n "$jsons" ]]; then
        add_finding 'D.3' 'config is TOML' 'WARN' '' \
            "Found $(echo "$jsons" | wc -l) config.json file(s)."
    else
        add_finding 'D.3' 'config is TOML' 'WARN' '' \
            "No edge_monitor.toml.example present. Confirm config story."
    fi

    # D.4 — ratatui in deps
    local has_ratatui
    has_ratatui=$(grep -rh 'ratatui' --include='Cargo.toml' . 2>/dev/null | grep -v '#' || true)
    save_evidence "D4_ratatui" "$has_ratatui" >/dev/null
    if [[ -n "$has_ratatui" ]]; then
        add_finding 'D.4' 'ratatui dependency present' 'PASS'
    else
        add_finding 'D.4' 'ratatui dependency present' 'FAIL' 'S2' \
            "ratatui not found in any Cargo.toml. TUI may use raw escapes."
    fi

    # D.5 — raw ANSI escapes in production source
    local ansi_hits
    ansi_hits=$(grep -rl -E '\\x1b\[|\\e\[' --include='*.rs' src crates 2>/dev/null \
        | grep -v '/tests/' || true)
    save_evidence "D5_ansi" "$ansi_hits" >/dev/null
    local ansi_count
    ansi_count=$(echo "$ansi_hits" | grep -c . || echo 0)

    if [[ $ansi_count -eq 0 ]]; then
        add_finding 'D.5' 'no raw ANSI escapes in production' 'PASS'
    elif [[ $ansi_count -le 2 ]]; then
        add_finding 'D.5' 'raw ANSI escapes in production' 'WARN' '' \
            "$ansi_count files. Could be legitimate (tracing color)."
    else
        add_finding 'D.5' 'raw ANSI escapes in production' 'FAIL' 'S2' \
            "$ansi_count files use raw ANSI. Suggests TUI not ported to ratatui."
    fi
}

# ---------------------------------------------------------------------------
# Phase E: CLI surface
# ---------------------------------------------------------------------------

test_cli_surface() {
    echo
    echo "${C_CYAN}=== Phase E: CLI surface ===${C_RESET}"

    local help_out
    help_out=$("$EXE" --help 2>&1)
    save_evidence "E1_help" "$help_out" >/dev/null

    # E.1 — history subcommand
    if echo "$help_out" | grep -q 'history'; then
        local hist_help
        hist_help=$("$EXE" history --help 2>&1 || true)
        save_evidence "E1_history_help" "$hist_help" >/dev/null
        add_finding 'E.1' 'history subcommand exists (Tier 1.1)' 'PASS'
    else
        add_finding 'E.1' 'history subcommand exists (Tier 1.1)' 'FAIL' 'S1' \
            "Tier 1.1 history feature claim not backed by CLI."
    fi

    # E.2 — compare
    if echo "$help_out" | grep -q 'compare'; then
        save_evidence "E2_compare_help" "$("$EXE" compare --help 2>&1 || true)" >/dev/null
        add_finding 'E.2' 'compare subcommand exists' 'PASS'
    else
        add_finding 'E.2' 'compare subcommand exists' 'WARN' '' \
            "Tier 3 feature; may not be implemented yet."
    fi

    # E.3 — exec wrapper
    if echo "$help_out" | grep -q 'exec'; then
        add_finding 'E.3' 'exec wrapper subcommand (Tier 1.2)' 'PASS'
    else
        add_finding 'E.3' 'exec wrapper subcommand (Tier 1.2)' 'WARN' '' \
            "Required for stdout-parsing tok/s. May rely only on Prometheus scraping."
    fi

    # E.4 — --no-ui
    if echo "$help_out" | grep -qE '--no-ui|--headless'; then
        add_finding 'E.4' '--no-ui headless mode' 'PASS'
    else
        add_finding 'E.4' '--no-ui headless mode' 'FAIL' 'S2'
    fi

    # E.5 — --dry-run
    if echo "$help_out" | grep -q -- '--dry-run'; then
        add_finding 'E.5' '--dry-run flag (governor safety)' 'PASS'
    else
        add_finding 'E.5' '--dry-run flag' 'WARN' '' \
            "May be config-only. Spec requires dry-run as default behavior."
    fi
}

# ---------------------------------------------------------------------------
# Phase F: Headless smoke test
# ---------------------------------------------------------------------------

test_headless_run() {
    echo
    echo "${C_CYAN}=== Phase F: Headless runtime ===${C_RESET}"

    # F.1 — --no-ui --ticks 5 with timeout
    local out
    if out=$(timeout 30 "$EXE" --no-ui --ticks 5 2>&1); then
        save_evidence "F1_headless_run" "$out" >/dev/null
        local tick_count
        tick_count=$(echo "$out" | grep -cE 'tick=|"tick"|tick #[0-9]+' || true)
        if [[ $tick_count -ge 5 ]]; then
            add_finding 'F.1' '--no-ui --ticks 5 produces ≥5 tick lines' 'PASS' '' \
                "$tick_count tick markers found"
        else
            add_finding 'F.1' '--no-ui --ticks 5 produces ≥5 tick lines' 'FAIL' 'S1' \
                "Only $tick_count tick markers. Headless mode silent or broken."
        fi
    else
        local rc=$?
        save_evidence "F1_headless_run" "Exit code: $rc\n$out" >/dev/null
        if [[ $rc -eq 124 ]]; then
            add_finding 'F.1' '--no-ui --ticks 5 exits within 30s' 'FAIL' 'S0' \
                "Timed out after 30s. Likely hang or infinite loop."
        else
            add_finding 'F.1' '--no-ui --ticks 5' 'FAIL' 'S1' \
                "Exit $rc. See F1_headless_run.txt."
        fi
    fi

    # F.2 — bad config rejected
    local bad_cfg="/tmp/bad_em_$$.toml"
    echo 'garbage{[' > "$bad_cfg"
    out=$("$EXE" --config "$bad_cfg" --no-ui --ticks 1 2>&1)
    local rc=$?
    rm -f "$bad_cfg"
    save_evidence "F2_bad_config" "Exit code: $rc\n$out" >/dev/null
    if [[ $rc -ne 0 ]]; then
        add_finding 'F.2' 'bad config rejected with non-zero exit' 'PASS'
    else
        add_finding 'F.2' 'bad config rejected' 'FAIL' 'S2' \
            "Bad TOML accepted silently. Config validation broken."
    fi
}

# ---------------------------------------------------------------------------
# Phase G: Prometheus exporter
# ---------------------------------------------------------------------------

test_prometheus() {
    echo
    echo "${C_CYAN}=== Phase G: Prometheus endpoint ===${C_RESET}"

    local cfg="/tmp/audit_em_$$.toml"
    cat > "$cfg" <<EOF
[runtime]
tick_interval_ms = 1000

[telemetry]
prometheus_bind = "127.0.0.1:9472"

[policy]
enforce = false
EOF

    "$EXE" --config "$cfg" --no-ui --ticks 30 \
        > /tmp/em_audit_stdout_$$ 2> /tmp/em_audit_stderr_$$ &
    local pid=$!
    sleep 5  # let it bind

    if curl -sS --max-time 5 http://127.0.0.1:9472/metrics > /tmp/metrics_$$ 2>&1; then
        local body
        body=$(cat /tmp/metrics_$$)
        save_evidence "G1_metrics_response" "$body" >/dev/null

        local metric_lines help_lines type_lines
        metric_lines=$(echo "$body" | grep -cE '^[a-z_]+(\{[^}]*\})?[[:space:]]+[0-9.eE+-]+' || true)
        help_lines=$(echo "$body" | grep -c '^# HELP' || true)
        type_lines=$(echo "$body" | grep -c '^# TYPE' || true)

        if [[ $metric_lines -ge 5 && $help_lines -ge 1 ]]; then
            add_finding 'G.1' 'Prometheus /metrics serves valid output' 'PASS' '' \
                "$metric_lines metric lines, $help_lines HELP, $type_lines TYPE"
        else
            add_finding 'G.1' 'Prometheus /metrics serves valid output' 'FAIL' 'S1' \
                "Endpoint responds but content sparse: $metric_lines metrics, $help_lines HELP."
        fi
    else
        save_evidence "G1_metrics_error" "$(cat /tmp/metrics_$$ 2>/dev/null || true)" >/dev/null
        add_finding 'G.1' 'Prometheus /metrics serves valid output' 'FAIL' 'S1' \
            "Endpoint did not respond. Either feature not implemented or port not bound."
    fi

    # cleanup
    kill "$pid" 2>/dev/null || true
    wait "$pid" 2>/dev/null || true
    rm -f "$cfg" /tmp/em_audit_stdout_$$ /tmp/em_audit_stderr_$$ /tmp/metrics_$$
}

# ---------------------------------------------------------------------------
# Phase H: Grafana dashboards
# ---------------------------------------------------------------------------

test_grafana() {
    echo
    echo "${C_CYAN}=== Phase H: Grafana dashboards ===${C_RESET}"

    local grafana_dir=""
    for c in grafana dashboards grafana_dashboards; do
        [[ -d "$c" ]] && grafana_dir="$c" && break
    done

    if [[ -z "$grafana_dir" ]]; then
        add_finding 'H.1' 'Grafana dashboards directory exists' 'SKIP' '' \
            "No grafana/ or dashboards/ directory found."
        return
    fi

    local jsons
    jsons=$(find "$grafana_dir" -maxdepth 2 -name '*.json' 2>/dev/null || true)
    save_evidence "H_grafana_files" "$jsons" >/dev/null

    if [[ -z "$jsons" ]]; then
        add_finding 'H.1' 'Grafana JSON files present' 'FAIL' 'S2'
        return
    fi

    local count
    count=$(echo "$jsons" | wc -l)
    add_finding 'H.1' 'Grafana JSON files present' 'PASS' '' "$count files"

    # H.2 — each is valid JSON
    local invalid=()
    local all_metrics=""
    while IFS= read -r j; do
        [[ -z "$j" ]] && continue
        if ! python3 -c "import json,sys; json.load(open('$j'))" 2>/dev/null; then
            invalid+=("$(basename "$j")")
            continue
        fi
        # extract edge_monitor_* metrics referenced
        local metrics_in_file
        metrics_in_file=$(grep -oE 'edge_monitor_[a-z_]+' "$j" 2>/dev/null | sort -u || true)
        all_metrics="$all_metrics
$metrics_in_file"
    done <<< "$jsons"

    if [[ ${#invalid[@]} -eq 0 ]]; then
        add_finding 'H.2' 'all dashboard JSON parses' 'PASS'
    else
        add_finding 'H.2' 'all dashboard JSON parses' 'FAIL' 'S2' \
            "Invalid: ${invalid[*]}"
    fi

    # H.3 — metrics referenced in dashboards exist in actual /metrics output
    local unique_metrics
    unique_metrics=$(echo "$all_metrics" | sort -u | grep -v '^$' || true)
    local metrics_file="$EVIDENCE_DIR/G1_metrics_response.txt"

    if [[ -f "$metrics_file" ]]; then
        local missing=() found=()
        while IFS= read -r m; do
            [[ -z "$m" ]] && continue
            if grep -qF "$m" "$metrics_file"; then
                found+=("$m")
            else
                missing+=("$m")
            fi
        done <<< "$unique_metrics"
        save_evidence "H3_metric_coverage" \
            "Found: ${found[*]}\n\nMissing: ${missing[*]}" >/dev/null

        if [[ ${#missing[@]} -eq 0 ]]; then
            add_finding 'H.3' 'dashboard metrics all exist in /metrics' 'PASS' '' \
                "${#found[@]} metrics referenced, all present"
        else
            add_finding 'H.3' 'dashboard metrics all exist in /metrics' 'FAIL' 'S1' \
                "${#missing[@]} metric(s) referenced in dashboards but NOT exported: ${missing[*]}"
        fi
    else
        add_finding 'H.3' 'dashboard metrics all exist in /metrics' 'SKIP' '' \
            "Phase G did not produce metrics output."
    fi
}

# ---------------------------------------------------------------------------
# Phase I: Documentation drift
# ---------------------------------------------------------------------------

test_documentation() {
    echo
    echo "${C_CYAN}=== Phase I: Documentation drift ===${C_RESET}"

    for doc in CLAUDE.md HANDOFF.md README.md FEATURES.md; do
        if [[ -f "$doc" ]]; then
            local mtime
            mtime=$(stat -c %Y "$doc" 2>/dev/null || stat -f %m "$doc" 2>/dev/null)
            local now age_days
            now=$(date +%s)
            age_days=$(awk -v n="$now" -v m="$mtime" 'BEGIN{printf "%.1f", (n-m)/86400}')
            local id="I.1.${doc%.md}"
            if awk -v a="$age_days" 'BEGIN{exit !(a > 14)}'; then
                add_finding "$id" "$doc freshness" 'WARN' '' \
                    "$age_days days old. Likely stale."
            else
                add_finding "$id" "$doc freshness" 'PASS' '' "$age_days days old"
            fi
        fi
    done

    # I.2 — git log: small-diff commits with completion-claim messages
    if git log -1 >/dev/null 2>&1; then
        local gl
        gl=$(git log --pretty=format:"%h%x09%s" --shortstat -n 20 2>&1)
        save_evidence "I2_git_log" "$gl" >/dev/null

        # parse commit→stat pairs
        local suspicious=()
        local current_hash="" current_msg=""
        while IFS= read -r line; do
            if [[ "$line" =~ ^([a-f0-9]+)$'\t'(.+)$ ]]; then
                current_hash="${BASH_REMATCH[1]}"
                current_msg="${BASH_REMATCH[2]}"
            elif [[ "$line" =~ insertion|deletion ]]; then
                local inserts=0 deletes=0
                if [[ "$line" =~ ([0-9]+)\ insertion ]]; then
                    inserts="${BASH_REMATCH[1]}"
                fi
                if [[ "$line" =~ ([0-9]+)\ deletion ]]; then
                    deletes="${BASH_REMATCH[1]}"
                fi
                local total=$((inserts + deletes))
                if [[ $total -lt 10 ]] && \
                   echo "$current_msg" | grep -qiE 'complete|done|ready|working|fix|implement'; then
                    suspicious+=("$current_hash : $current_msg ($total lines)")
                fi
            fi
        done <<< "$gl"

        if [[ ${#suspicious[@]} -eq 0 ]]; then
            add_finding 'I.2' 'no misleading commit messages in last 20' 'PASS'
        else
            local example="${suspicious[0]}"
            add_finding 'I.2' 'misleading commit messages flagged' 'WARN' '' \
                "${#suspicious[@]} suspicious commits. Example: $example"
        fi
    else
        add_finding 'I.2' 'git log accessible' 'SKIP' '' "Not a git repo."
    fi
}

# ---------------------------------------------------------------------------
# Phase J: Ground-truth tokens/sec
# ---------------------------------------------------------------------------

test_ground_truth_tokens() {
    if [[ $SKIP_SLOW -eq 1 ]]; then
        add_finding 'J.1' 'tokens/sec ground-truth vs Ollama' 'SKIP' '' \
            "--skip-slow specified."
        return
    fi

    echo
    echo "${C_CYAN}=== Phase J: Ground-truth tokens/sec test (slow) ===${C_RESET}"

    if ! command -v ollama >/dev/null 2>&1; then
        add_finding 'J.1' 'tokens/sec ground-truth' 'SKIP' '' \
            "Ollama not installed. https://ollama.com to enable T.1 ground-truth check."
        return
    fi

    echo "${C_GRAY}  Running Ollama generation (~30s)...${C_RESET}"
    local out
    out=$(ollama run --verbose phi3 "Explain quicksort in 200 words. Be detailed." 2>&1)
    save_evidence "J1_ollama_output" "$out" >/dev/null

    if [[ "$out" =~ eval\ rate:[[:space:]]+([0-9.]+)\ tokens/s ]]; then
        local tps="${BASH_REMATCH[1]}"
        add_finding 'J.1.ollama' 'Ollama eval rate captured' 'PASS' '' \
            "Ollama reports $tps tok/s"
        add_finding 'J.1.compare' 'edge_monitor vs Ollama tok/s comparison' 'SKIP' '' \
            "MANUAL: while Ollama runs, capture edge_monitor's tokens_per_sec_avg from latest run record. Pass if within 15% of $tps."
    else
        add_finding 'J.1' 'Ollama produced eval rate line' 'FAIL' 'S2' \
            "ollama --verbose did not contain 'eval rate:'."
    fi
}

# ---------------------------------------------------------------------------
# Report
# ---------------------------------------------------------------------------

write_report() {
    local total=${#F_ID[@]}
    local passed=0 failed=0 warned=0 skipped=0
    for s in "${F_STATUS[@]}"; do
        case "$s" in
            PASS) passed=$((passed+1)) ;;
            FAIL) failed=$((failed+1)) ;;
            WARN) warned=$((warned+1)) ;;
            SKIP) skipped=$((skipped+1)) ;;
        esac
    done

    local end_time duration_min
    end_time=$(date +%s)
    duration_min=$(awk -v s="$START_TIME" -v e="$end_time" 'BEGIN{printf "%.1f", (e-s)/60}')

    local report="$OUTPUT_DIR/REPORT.md"
    {
        echo "# edge_monitor — Linux Audit Report"
        echo
        echo "- **Generated:** $(date '+%Y-%m-%d %H:%M:%S')"
        echo "- **Duration:** $duration_min minutes"
        echo "- **Project:** $PROJECT_ROOT"
        echo "- **Evidence:** \`$EVIDENCE_DIR\`"
        echo
        echo "## Summary"
        echo
        echo "| Result | Count |"
        echo "|---|---|"
        echo "| PASS | $passed |"
        echo "| FAIL | $failed |"
        echo "| WARN | $warned |"
        echo "| SKIP | $skipped |"
        echo "| **Total** | **$total** |"
        echo

        # Launch blockers
        local blocker_count=0
        for ((i=0; i<total; i++)); do
            if [[ "${F_STATUS[$i]}" == "FAIL" ]] && [[ "${F_SEV[$i]}" =~ ^S[01]$ ]]; then
                blocker_count=$((blocker_count+1))
            fi
        done

        if [[ $blocker_count -gt 0 ]]; then
            echo "## ⚠ Launch blockers"
            echo
            echo "$blocker_count S0/S1 failure(s). Cannot launch until resolved."
            echo
            for ((i=0; i<total; i++)); do
                if [[ "${F_STATUS[$i]}" == "FAIL" ]] && [[ "${F_SEV[$i]}" =~ ^S[01]$ ]]; then
                    echo "- **[${F_ID[$i]}]** (${F_SEV[$i]}) ${F_DESC[$i]} — ${F_NOTES[$i]}"
                fi
            done
            echo
        elif [[ $failed -eq 0 && $warned -lt 3 ]]; then
            echo "## ✓ No launch blockers found"
            echo
            echo "Audit clean. Verify WARN items, run ground-truth (Phase J) manually if SKIP'd."
            echo
        fi

        echo "## Findings"
        echo
        echo "| ID | Status | Sev | Description | Notes |"
        echo "|---|---|---|---|---|"
        for ((i=0; i<total; i++)); do
            local emoji
            case "${F_STATUS[$i]}" in
                PASS) emoji="✓" ;;
                FAIL) emoji="✗" ;;
                WARN) emoji="⚠" ;;
                SKIP) emoji="—" ;;
            esac
            local notes
            notes=$(echo "${F_NOTES[$i]}" | tr '\n' ' ' | sed 's/|/\\|/g')
            echo "| ${F_ID[$i]} | $emoji ${F_STATUS[$i]} | ${F_SEV[$i]} | ${F_DESC[$i]} | $notes |"
        done

        echo
        echo "## Reproduction"
        echo
        echo '```bash'
        echo "./verify-edge-monitor.sh --project-root '$PROJECT_ROOT'"
        echo '```'
        echo
        echo "Raw evidence in \`$EVIDENCE_DIR\`."
    } > "$report"

    echo
    echo "${C_CYAN}================================================${C_RESET}"
    echo "${C_CYAN}Audit complete in $duration_min minutes${C_RESET}"
    echo "  PASS: $passed   FAIL: $failed   WARN: $warned   SKIP: $skipped"
    echo "  Report: $report"
    echo "  Evidence: $EVIDENCE_DIR"
    echo "${C_CYAN}================================================${C_RESET}"

    if [[ $blocker_count -gt 0 ]]; then
        echo
        echo "${C_RED}⚠ $blocker_count S0/S1 LAUNCH BLOCKER(S) — see report${C_RESET}"
        exit 1
    fi
}

# ---------------------------------------------------------------------------
# Main
# ---------------------------------------------------------------------------

initialize_audit

if ! test_build_and_clippy; then
    echo
    echo "${C_RED}Build failed; skipping subsequent phases.${C_RESET}"
    write_report
    exit 1
fi

test_suite
test_production_quality
test_identity_unification
test_cli_surface
test_headless_run
test_prometheus
test_grafana
test_documentation
test_ground_truth_tokens

write_report
