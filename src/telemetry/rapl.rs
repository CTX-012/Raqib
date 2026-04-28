//! Intel RAPL package-power reader (latest.md Tier 2.1).
//!
//! RAPL exposes a monotonically-increasing `energy_uj` counter at
//! `/sys/class/powercap/intel-rapl:<N>/energy_uj` (microjoules).
//! Average power over a window is `(Δenergy_uj / 1e6) / Δt`.
//!
//! Counters wrap at the value reported in `max_energy_range_uj`
//! (typically ~262 GJ on modern Xeon, but only ~262 J on some legacy
//! parts where the counter is 32-bit). We handle wraparound by
//! detecting the case `e2 < e1` and adding `max_range` before
//! subtracting — without that, a wrap would surface as a momentarily
//! enormous negative wattage.
//!
//! Reading is permission-gated: `energy_uj` is sometimes mode 0400
//! (root-only) on hardened distributions. We treat that as "RAPL
//! unavailable" and emit a single warn log; the caller falls back to
//! `None` watts.

use std::fs;
use std::path::{Path, PathBuf};
use std::time::Instant;

const RAPL_ROOT: &str = "/sys/class/powercap";

/// One RAPL package. Multi-socket boxes have several; sum across
/// packages to get the system total.
#[derive(Debug, Clone)]
struct RaplPackage {
    energy_path: PathBuf,
    /// `max_energy_range_uj` from sysfs, used to detect wraparound.
    /// `0` means "no wraparound info" — caller should distrust large
    /// negative deltas in that case.
    max_range_uj: u64,
}

/// Stateful reader. Holds the last `(energy_uj, instant)` per package
/// so successive `read_watts()` calls can compute Δ-based wattage.
pub struct RaplReader {
    packages: Vec<RaplPackage>,
    last: Option<(Vec<u64>, Instant)>,
    /// True after the first `warn` for unavailable RAPL — prevents log
    /// spam on every tick when the kernel says "no".
    warned_unavailable: bool,
}

impl RaplReader {
    /// Discover all `intel-rapl:N` packages. Returns a reader with no
    /// packages (so `read_watts()` always returns `None`) on hosts
    /// where `/sys/class/powercap` is missing or empty.
    pub fn new() -> Self {
        let packages = discover_packages(Path::new(RAPL_ROOT)).unwrap_or_default();
        Self {
            packages,
            last: None,
            warned_unavailable: false,
        }
    }

    /// True when at least one RAPL package was discovered. Used by
    /// the dispatcher to skip the read entirely on AMD / non-Intel
    /// systems.
    pub fn available(&self) -> bool {
        !self.packages.is_empty()
    }

    /// Average package wattage since the previous successful call.
    /// Returns `None` on the first call (no Δ window yet) or when
    /// reading fails.
    pub fn read_watts(&mut self) -> Option<f32> {
        if self.packages.is_empty() {
            return None;
        }
        let now = Instant::now();
        let mut current: Vec<u64> = Vec::with_capacity(self.packages.len());
        for pkg in &self.packages {
            match fs::read_to_string(&pkg.energy_path) {
                Ok(s) => match s.trim().parse::<u64>() {
                    Ok(uj) => current.push(uj),
                    Err(_) => return self.fail_read(),
                },
                Err(e) => {
                    if !self.warned_unavailable {
                        tracing::warn!(
                            path = %pkg.energy_path.display(),
                            error = %e,
                            "RAPL energy_uj unreadable; CPU watts unavailable"
                        );
                        self.warned_unavailable = true;
                    }
                    return None;
                }
            }
        }

        let prev = self.last.replace((current.clone(), now));
        let (prev_vals, prev_at) = prev?;
        if prev_vals.len() != current.len() {
            // Hot-pluggable RAPL packages would be a surprise; treat
            // shape change as a reset.
            return None;
        }
        let dt = now.saturating_duration_since(prev_at).as_secs_f32();
        if dt <= 0.0 {
            return None;
        }
        let mut delta_uj: u64 = 0;
        for (i, (&now_uj, &prev_uj)) in current.iter().zip(&prev_vals).enumerate() {
            let max_range = self.packages[i].max_range_uj;
            let d = if now_uj >= prev_uj {
                now_uj - prev_uj
            } else if max_range > 0 {
                // Wraparound: counter went prev_uj → max_range → 0 → now_uj.
                (max_range - prev_uj) + now_uj
            } else {
                // No max_range hint and counter went backwards. Most
                // likely a sysfs glitch; skip this delta.
                return None;
            };
            delta_uj = delta_uj.saturating_add(d);
        }
        let joules = delta_uj as f32 / 1.0e6;
        Some(joules / dt)
    }

    fn fail_read(&mut self) -> Option<f32> {
        self.last = None;
        None
    }
}

impl Default for RaplReader {
    fn default() -> Self {
        Self::new()
    }
}

fn discover_packages(root: &Path) -> std::io::Result<Vec<RaplPackage>> {
    let mut out = Vec::new();
    for entry in fs::read_dir(root)? {
        let entry = entry?;
        let name = match entry.file_name().into_string() {
            Ok(s) => s,
            Err(_) => continue,
        };
        // Top-level package directories start with `intel-rapl:`.
        // Sub-domains (`intel-rapl:0:0` for cores) are skipped — we
        // want package totals, not core/dram split.
        if !name.starts_with("intel-rapl:") || name.matches(':').count() != 1 {
            continue;
        }
        let energy = entry.path().join("energy_uj");
        if !energy.exists() {
            continue;
        }
        let max_range = fs::read_to_string(entry.path().join("max_energy_range_uj"))
            .ok()
            .and_then(|s| s.trim().parse::<u64>().ok())
            .unwrap_or(0);
        out.push(RaplPackage {
            energy_path: energy,
            max_range_uj: max_range,
        });
    }
    Ok(out)
}

#[cfg(test)]
mod tests {
    use super::*;

    /// On hosts without `/sys/class/powercap` (WSL2, BSD, macOS),
    /// the reader still constructs but reports unavailable.
    #[test]
    fn reader_constructs_even_without_rapl() {
        let r = RaplReader::new();
        // Existence of the dir varies by host — just verify the
        // constructor never panics and `available()` is consistent
        // with `packages.len()`.
        assert_eq!(r.available(), !r.packages.is_empty());
    }

    /// `read_watts()` on first call always returns None (no Δ window).
    #[test]
    fn first_read_returns_none() {
        let mut r = RaplReader {
            packages: vec![],
            last: None,
            warned_unavailable: false,
        };
        assert!(r.read_watts().is_none());
    }

    /// Δ math: 100 J in 1 s = 100 W. Build a fake reader with a fake
    /// energy path and verify the calculation by manually injecting
    /// state.
    #[test]
    fn delta_math_handles_normal_increment() {
        let tmp = tempfile::tempdir().unwrap();
        let pkg_dir = tmp.path().join("intel-rapl:0");
        fs::create_dir_all(&pkg_dir).unwrap();
        fs::write(pkg_dir.join("energy_uj"), "1000000000\n").unwrap(); // 1000 J
        fs::write(pkg_dir.join("max_energy_range_uj"), "262144000000\n").unwrap();

        let pkgs = discover_packages(tmp.path()).unwrap();
        assert_eq!(pkgs.len(), 1);
        let mut r = RaplReader {
            packages: pkgs,
            last: None,
            warned_unavailable: false,
        };
        // Prime: first read produces no answer but stores state.
        assert!(r.read_watts().is_none());
        // Bump by 100 J, sleep ~50 ms, expect ~2000 W (100 J / 0.05 s).
        // Use a wider tolerance because sleeps are imprecise.
        std::thread::sleep(std::time::Duration::from_millis(50));
        fs::write(tmp.path().join("intel-rapl:0/energy_uj"), "1100000000\n").unwrap();
        let w = r.read_watts().expect("watts");
        assert!(
            w > 100.0,
            "expected >100W from a 100J / ~50ms delta, got {w}"
        );
    }

    /// Wraparound: counter goes from near-max back to small, with
    /// max_range provided. Should give a small positive delta, not a
    /// huge negative one.
    #[test]
    fn delta_math_handles_wraparound() {
        let tmp = tempfile::tempdir().unwrap();
        let pkg_dir = tmp.path().join("intel-rapl:0");
        fs::create_dir_all(&pkg_dir).unwrap();
        // Counter near its max range.
        let max: u64 = 1_000_000_000; // 1000 J max range (synthetic small)
        fs::write(pkg_dir.join("energy_uj"), (max - 50_000_000).to_string()).unwrap(); // max - 50J
        fs::write(pkg_dir.join("max_energy_range_uj"), max.to_string()).unwrap();

        let pkgs = discover_packages(tmp.path()).unwrap();
        let mut r = RaplReader {
            packages: pkgs,
            last: None,
            warned_unavailable: false,
        };
        assert!(r.read_watts().is_none()); // prime
        std::thread::sleep(std::time::Duration::from_millis(20));
        // Counter wrapped; now at 50J past zero. Delta should be
        // (max - prev) + now = 50J + 50J = 100J.
        fs::write(tmp.path().join("intel-rapl:0/energy_uj"), "50000000\n").unwrap();
        let w = r.read_watts().expect("watts");
        // 100J in ~20ms → ~5000W. Sanity: positive and large.
        assert!(w > 1000.0, "wraparound delta produced {}W", w);
        assert!(w.is_finite());
    }
}
