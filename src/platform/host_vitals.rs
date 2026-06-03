//! v1.1.12 / DISPATCH 39 / CAR-22 — host-level vitals collection
//! (thermal-only in this release).
//!
//! Reads `/sys/class/thermal/thermal_zone*/{type,temp}` via
//! [`std::fs`], enumerates every zone the kernel exposes, and returns
//! a populated [`ux_contract::host_vitals::HostVitals`]. INA3221
//! per-rail power is deferred per the v1.1.12 dispatch.
//!
//! ## Why std::fs (and not `sysinfo`'s built-in temperature surface)
//!
//! `sysinfo` exposes temperatures via a `Components` interface that on
//! Linux ultimately reads the same `/sys/class/thermal/` files we
//! enumerate here — but it gates the read behind `Components::new()`
//! which the v1.1.8 ITEM 2 refactor pruned out of the long-lived
//! [`sysinfo::System`] for allocation reasons. Re-introducing it just
//! for the thermal read would reintroduce the per-tick allocation
//! pattern that DISPATCH 25 removed. A direct `std::fs::read_dir` on
//! `/sys/class/thermal/` allocates only the result vec and the zone
//! strings, scales linearly with zone count (Jetson Orin AGX has ~9,
//! Gigabyte B560M has ~3), and stays read-only — no syscall surface
//! beyond what the existing platform reads already use.
//!
//! ## Error model
//!
//! Per-zone errors (permission denied, mid-tick race against a kernel
//! sysfs rebuild, malformed temp value) skip THAT zone and continue.
//! `HostVitals { thermal_zones: Vec::new() }` is a valid empty case —
//! the contract docs explicitly state "empty means no zones discovered"
//! and the consumer hides the panel. This matches the RAPL "unreadable
//! → None" degradation pattern in
//! [`crate::telemetry::rapl::RaplReader`].
//!
//! ## Authority lock
//!
//! Observation-only. This module never writes to `/sys/class/thermal/`
//! or anywhere else, never spawns subprocesses, never reaches any
//! kill / signal path. Per the DISPATCH 36 / v1.1.11 authority lock,
//! Phase 3 stays observe-only.

use std::fs;
use std::path::{Path, PathBuf};

use ux_contract::host_vitals::{HostVitals, ThermalZone};

/// Root of the Linux thermal sysfs tree. Lifted to a const so the
/// test helpers can swap it for a tempdir via [`collect_from_root`].
const THERMAL_SYSFS_ROOT: &str = "/sys/class/thermal";

/// Collect the host's thermal zones from the canonical
/// `/sys/class/thermal/` sysfs tree. Always returns; an empty
/// `thermal_zones` vec is a valid response on hosts without thermal
/// sensors or on alien `/proc`-style mounts where every read fails.
///
/// Production callers (the platform-layer collection in
/// [`crate::platform::collect_snapshot`]) use this entry. Tests use
/// `collect_from_root` (crate-private) with a tempdir.
pub fn collect_host_vitals() -> HostVitals {
    // v1.3.0 / DISPATCH 50 — `EDGE_MONITOR_THERMAL_ROOT` env override
    // redirects the thermal sysfs root. Unblocks Jetson-deferred
    // validation on x86: point the var at a tempdir of synthetic
    // `thermal_zoneN/{type,temp}` files and the rest of the alert /
    // recommendation path (v1.1.12 vitals → v1.2.0 ThermalPressure
    // rec) fires end-to-end without real hot hardware. Unset =
    // `/sys/class/thermal` (current behaviour). Invalid path =
    // `collect_from_root` returns empty per its existing "no zones
    // discovered" semantics — no crash, no panic.
    let root = std::env::var_os("EDGE_MONITOR_THERMAL_ROOT")
        .map(std::path::PathBuf::from)
        .unwrap_or_else(|| std::path::PathBuf::from(THERMAL_SYSFS_ROOT));
    collect_from_root(&root)
}

/// Variant of [`collect_host_vitals`] that reads zones from an
/// arbitrary root path. Production calls
/// [`collect_host_vitals`] which delegates here with the real sysfs
/// root; tests pass a tempdir with synthetic `thermal_zone*` entries.
///
/// `pub(crate)` because the only legitimate non-test caller is the
/// production wrapper above; downstream consumers should not bypass
/// the `/sys/class/thermal/` lock-in.
pub(crate) fn collect_from_root(root: &Path) -> HostVitals {
    let Ok(entries) = fs::read_dir(root) else {
        // Root unreadable (missing sysfs, permission denied at the
        // root, alien `/proc` mount). Return empty per the contract's
        // "no zones discovered" semantics.
        // v0.3.16 contract grew `power_rails`; INA3221 collection
        // lands in v1.3.3. Until then the consumer always returns an
        // empty rails vec — same shape the contract documents for
        // hosts without an INA3221 driver.
        return HostVitals {
            thermal_zones: Vec::new(),
            power_rails: Vec::new(),
        };
    };

    let mut zones: Vec<ThermalZone> = Vec::new();
    for entry in entries.flatten() {
        let name = entry.file_name();
        let Some(name_str) = name.to_str() else {
            continue;
        };
        // Match `thermal_zone*` — the only sysfs entries that expose
        // a `(type, temp)` pair. `cooling_device*` is unrelated.
        if !name_str.starts_with("thermal_zone") {
            continue;
        }
        let zone_dir = entry.path();
        if let Some(zone) = read_one_zone(&zone_dir) {
            zones.push(zone);
        }
        // Per-zone read failures: silently skip. The "skip vs surface
        // an empty zone" choice matches the RAPL degradation pattern.
        // A future tracing hook could log the skip at TRACE level if
        // an operator wants to count unreadable zones.
    }

    // Stable order: sort by label so consumers (TUI top-3 selector,
    // web rendering) get reproducible output across ticks. The kernel
    // doesn't guarantee a particular `read_dir` order on every
    // filesystem, and the v1.1.5 fixture-asymmetry discipline argues
    // for canonicalising at the producer.
    zones.sort_by(|a, b| a.label.cmp(&b.label));

    HostVitals {
        thermal_zones: zones,
        // INA3221 collection deferred to v1.3.3 (ux_contract
        // v0.3.16 landed the type; consumption follows in a later
        // sub-version). Empty vec is the contract's valid "no
        // rails discovered" state for x86 and any host without an
        // INA3221 driver.
        power_rails: Vec::new(),
    }
}

/// Read one zone directory's `type` (label) and `temp` (millidegrees
/// Celsius) files. Returns `None` if either read fails or the temp
/// value isn't a parsable integer — the caller skips that zone.
///
/// Public for test fixtures that want to drive a single zone without
/// constructing a whole `read_dir`-able tree.
pub(crate) fn read_one_zone(zone_dir: &Path) -> Option<ThermalZone> {
    let type_path = zone_dir.join("type");
    let temp_path = zone_dir.join("temp");

    let label = fs::read_to_string(&type_path).ok()?.trim().to_string();
    if label.is_empty() {
        return None;
    }
    let temp_raw = fs::read_to_string(&temp_path).ok()?;
    let temp_millideg: i64 = temp_raw.trim().parse().ok()?;
    // Kernel reports millidegrees Celsius (e.g. `47200` = 47.2 °C).
    // f32 is enough precision; see `ux_contract::host_vitals` docs.
    let temp_celsius = temp_millideg as f32 / 1000.0;

    // Defensive: a wildly negative reading suggests a sensor in an
    // error state. Most sensors report `0` or a small positive at
    // boot. We don't filter — the contract is "raw values, consumer
    // classifies" — but if a future need arises this is the seam.

    Some(ThermalZone {
        label,
        temp_celsius,
    })
}

/// Helper for tests: build a synthetic thermal_zone directory at
/// `root/thermal_zoneN` with the given label + temperature. The
/// temperature is expressed in degrees Celsius (the helper does the
/// millidegrees conversion to match the kernel's on-disk format).
///
/// `pub(crate)` so it's reachable from the test module but not part
/// of any public API.
#[cfg(test)]
pub(crate) fn write_zone_fixture(
    root: &Path,
    index: usize,
    label: &str,
    temp_celsius: f32,
) -> PathBuf {
    let zone_dir = root.join(format!("thermal_zone{index}"));
    fs::create_dir_all(&zone_dir).expect("create zone dir");
    fs::write(zone_dir.join("type"), label).expect("write type");
    let temp_millideg = (temp_celsius * 1000.0) as i64;
    fs::write(zone_dir.join("temp"), temp_millideg.to_string()).expect("write temp");
    zone_dir
}

#[cfg(not(test))]
#[allow(dead_code)]
fn _unused_pathbuf_placeholder(_: PathBuf) {} // suppresses unused import warning on non-test builds

#[cfg(test)]
mod tests {
    use super::*;
    use tempfile::TempDir;

    /// Asymmetric-fixture discipline: real sysfs has multiple zones
    /// with distinct labels and a wide temp range. Drive the
    /// production read path against a tempdir holding the same
    /// shape and assert every zone surfaces.
    #[test]
    fn host_vitals_reads_thermal_zones() {
        let tmp = TempDir::new().expect("tempdir");
        write_zone_fixture(tmp.path(), 0, "x86_pkg_temp", 47.2);
        write_zone_fixture(tmp.path(), 1, "acpitz", 53.0);
        write_zone_fixture(tmp.path(), 2, "TCPU", 38.5);

        let vitals = collect_from_root(tmp.path());

        assert_eq!(
            vitals.thermal_zones.len(),
            3,
            "all three fixture zones must surface",
        );
        // Stable-sort means we can pin order.
        let labels: Vec<&str> = vitals
            .thermal_zones
            .iter()
            .map(|z| z.label.as_str())
            .collect();
        assert_eq!(labels, vec!["TCPU", "acpitz", "x86_pkg_temp"]);
        // Spot-check the conversion (millidegrees → Celsius).
        let acpitz = vitals
            .thermal_zones
            .iter()
            .find(|z| z.label == "acpitz")
            .unwrap();
        assert!((acpitz.temp_celsius - 53.0).abs() < 0.01);
    }

    /// Per-zone read failure (missing `temp` file simulating a
    /// permission-denied or race-with-kernel-rebuild) MUST be
    /// skipped silently and not abort the whole snapshot. This is
    /// the RAPL "unreadable; unavailable" degradation pattern —
    /// the platform layer's other reads (NVML, sysinfo, /proc)
    /// follow the same shape.
    #[test]
    fn host_vitals_skips_unreadable_zone() {
        let tmp = TempDir::new().expect("tempdir");
        write_zone_fixture(tmp.path(), 0, "x86_pkg_temp", 47.2);
        // Create a half-broken zone: directory exists with `type` but
        // no `temp` file. Reads of the missing file return `Err`.
        let broken_dir = tmp.path().join("thermal_zone1");
        fs::create_dir_all(&broken_dir).unwrap();
        fs::write(broken_dir.join("type"), "broken_zone").unwrap();
        // No `temp` written — read_to_string returns NotFound.

        let vitals = collect_from_root(tmp.path());

        assert_eq!(
            vitals.thermal_zones.len(),
            1,
            "broken zone must be skipped; the readable one survives",
        );
        assert_eq!(vitals.thermal_zones[0].label, "x86_pkg_temp");
    }

    /// Empty `/sys/class/thermal/` (no `thermal_zone*` entries) is a
    /// valid case — e.g. running inside a container without
    /// /sys-mount or on a CPU-only headless box that the kernel
    /// hasn't exposed thermal trips for. Returns
    /// `HostVitals { thermal_zones: vec![] }` per the contract's
    /// "empty means no zones discovered" semantics. The consumer
    /// hides the panel rather than rendering an empty section.
    #[test]
    fn host_vitals_no_zones_returns_empty() {
        let tmp = TempDir::new().expect("tempdir");
        // No fixture writes — tempdir is empty.
        let vitals = collect_from_root(tmp.path());
        assert!(
            vitals.thermal_zones.is_empty(),
            "empty sysfs root must produce an empty thermal_zones vec",
        );
    }

    /// Missing sysfs root entirely (path doesn't exist) is the
    /// alien-`/proc` / container-without-sysfs case. Must NOT panic.
    #[test]
    fn host_vitals_missing_root_returns_empty() {
        let nonexistent = std::path::PathBuf::from("/var/empty/no-such-thermal-root");
        let vitals = collect_from_root(&nonexistent);
        assert!(vitals.thermal_zones.is_empty());
    }

    /// `cooling_device*` siblings of `thermal_zone*` must NOT be
    /// picked up. The kernel exposes cooling devices (fans,
    /// passive trips) in the same `/sys/class/thermal/` directory,
    /// and a naive `read_dir` would pick them up as zones. The
    /// `starts_with("thermal_zone")` filter pins the contract.
    #[test]
    fn host_vitals_ignores_cooling_devices() {
        let tmp = TempDir::new().expect("tempdir");
        write_zone_fixture(tmp.path(), 0, "x86_pkg_temp", 47.2);
        // A cooling_device entry exists at the same level. Naive
        // enumeration would attempt to read its `type` (a label
        // like "Processor") and `temp` (which doesn't exist).
        let cooling_dir = tmp.path().join("cooling_device0");
        fs::create_dir_all(&cooling_dir).unwrap();
        fs::write(cooling_dir.join("type"), "Processor").unwrap();

        let vitals = collect_from_root(tmp.path());

        assert_eq!(vitals.thermal_zones.len(), 1);
        assert_eq!(vitals.thermal_zones[0].label, "x86_pkg_temp");
    }

    /// v1.3.0 / DISPATCH 50 — `EDGE_MONITOR_THERMAL_ROOT` redirects
    /// `collect_host_vitals` from `/sys/class/thermal` to an
    /// arbitrary path. Unblocks Jetson-deferred validation on x86:
    /// a Tester (or operator on a cold dev host) points the var at a
    /// tempdir of synthetic `thermal_zoneN/{type,temp}` fixtures and
    /// drives the v1.1.12 thermal alert + v1.2.0 ThermalPressure rec
    /// path end-to-end without real hot hardware.
    ///
    /// This test covers BOTH the redirect AND the
    /// "invalid override degrades to empty" path in one process so
    /// the env-var teardown happens once (env vars are process-global
    /// and pollute parallel tests if leaked).
    #[test]
    fn thermal_root_env_override_redirects_collection() {
        const ENV_VAR: &str = "EDGE_MONITOR_THERMAL_ROOT";

        let tmp = TempDir::new().expect("tempdir");
        write_zone_fixture(tmp.path(), 0, "synthetic_pkg", 90.5);

        // SAFETY: env-var manipulation is process-global. This test
        // is the only one in the suite that touches the var, so the
        // window between set / read / restore can't race with
        // another test. The prior-value save+restore at end keeps a
        // pre-existing EDGE_MONITOR_THERMAL_ROOT in the harness env
        // intact for any caller that wrapped `cargo test` with it.
        let prior = std::env::var_os(ENV_VAR);

        unsafe { std::env::set_var(ENV_VAR, tmp.path()); }
        let redirected = collect_host_vitals();

        unsafe { std::env::set_var(ENV_VAR, "/var/empty/no-such-thermal-root"); }
        let degraded = collect_host_vitals();

        match prior {
            Some(v) => unsafe { std::env::set_var(ENV_VAR, &v); },
            None => unsafe { std::env::remove_var(ENV_VAR); },
        }

        // Assertions after restore so a failure can't leave the env
        // polluted for subsequent tests.
        assert_eq!(
            redirected.thermal_zones.len(),
            1,
            "override path must surface its single synthetic zone",
        );
        assert_eq!(redirected.thermal_zones[0].label, "synthetic_pkg");
        assert!(
            (redirected.thermal_zones[0].temp_celsius - 90.5).abs() < 0.01,
            "synthetic 90.5 °C must round-trip via the millidegrees \
             fixture (got {})",
            redirected.thermal_zones[0].temp_celsius,
        );

        assert!(
            degraded.thermal_zones.is_empty(),
            "invalid override path must degrade to empty thermal_zones \
             per `collect_from_root`'s `read_dir` Err arm; got {} zones",
            degraded.thermal_zones.len(),
        );
    }

    /// Millidegrees → Celsius conversion pinned. Kernel reports
    /// integer millidegrees; the f32 result must agree with the
    /// expected float value within 0.01 °C.
    #[test]
    fn host_vitals_converts_millidegrees_to_celsius() {
        let tmp = TempDir::new().expect("tempdir");
        write_zone_fixture(tmp.path(), 0, "x86_pkg_temp", 47.2);
        let vitals = collect_from_root(tmp.path());
        let z = &vitals.thermal_zones[0];
        assert!(
            (z.temp_celsius - 47.2).abs() < 0.01,
            "47.2 °C round-trip via millidegrees lossless within 0.01 °C \
             (got {})",
            z.temp_celsius,
        );
    }
}
