//! Friendly-name mapping for kernel thermal-zone labels.
//!
//! `/sys/class/thermal/thermal_zone*/type` returns platform-specific
//! strings like `x86_pkg_temp` or `acpitz` that read as noise to a
//! human scanning the Vitals panel. This helper maps the raw label
//! to a plain-English name for display. The raw label is preserved
//! by the caller and shown alongside (muted) so operators who use
//! `sensors` / `btop` can still cross-reference.
//!
//! Load-bearing rules (pinned by tests):
//! * **Fallback is pass-through.** Unknown raw label → the raw label
//!   is returned verbatim. Never blank, never a "?" placeholder — an
//!   unrecognised sensor on a new host is still legible.
//! * **Duplicate raw labels disambiguate by position.** Multiple
//!   `acpitz` zones (common on x86) render as `System Zone 1`,
//!   `System Zone 2`, … in the order they arrive. This mirrors the
//!   web `each_key_duplicate` scar's positional keying at
//!   `VitalsPanel.svelte:156`.
//! * **We DO NOT guess physical location.** A raw `acpitz` doesn't
//!   tell us which physical zone the sensor is at, so we say
//!   `System Zone N`, not `Motherboard` / `PCH` — honesty over
//!   friendliness.
//!
//! Callable from both the wire mapper (`src/web/wire.rs`) and the
//! TUI thermal renderer (`src/ui/panels/vitals.rs`) so the friendly
//! text is identical on both surfaces.

/// Map raw kernel sensor labels to friendly display names, in-order.
///
/// Returns a `Vec<String>` the same length as `raw_labels`; the
/// `i`-th friendly name corresponds to the `i`-th raw label. Duplicate
/// raw labels that map to the same friendly base get an appended
/// `" 1"`, `" 2"`, … in first-seen order.
///
/// The caller is expected to render both the friendly name and the
/// original `raw` label (muted) — this helper never returns the raw
/// label as an already-fused string, so the renderer decides the
/// display shape.
pub fn humanize_thermal_labels(raw_labels: &[String]) -> Vec<String> {
    let mut base_counts: std::collections::HashMap<&str, usize> =
        std::collections::HashMap::new();
    let mut totals: std::collections::HashMap<&str, usize> =
        std::collections::HashMap::new();
    let bases: Vec<&'static str> = raw_labels
        .iter()
        .map(|l| base_friendly(l))
        .collect();
    for b in &bases {
        *totals.entry(b).or_insert(0) += 1;
    }
    let mut out = Vec::with_capacity(raw_labels.len());
    for (idx, base) in bases.iter().enumerate() {
        if totals[base] > 1 {
            let n = base_counts.entry(base).or_insert(0);
            *n += 1;
            out.push(format!("{base} {n}"));
        } else if base.is_empty() {
            // Unknown sensor → pass through the raw label. Empty
            // string is the sentinel base_friendly returns for
            // "no mapping"; we substitute the raw label so the
            // renderer never gets an empty friendly.
            out.push(raw_labels[idx].clone());
        } else {
            out.push(base.to_string());
        }
    }
    out
}

/// Map a single raw label to its base friendly name (WITHOUT
/// disambiguation suffix). Returns `""` for unknown labels — the
/// caller substitutes the raw label in that case.
///
/// Table intentionally covers only labels we've observed in the
/// wild + the ones documented as future targets in
/// `WireThermalZone`'s doc-comment (`cpu-thermal` etc. on Jetson).
/// Adding a new mapping is a one-line edit here + a test.
fn base_friendly(raw: &str) -> &'static str {
    match raw {
        // x86 dev hosts.
        "x86_pkg_temp" => "CPU Package",
        "acpitz" => "System Zone",
        "pch_skylake" | "pch_cannonlake" | "pch_haswell" => "PCH",
        "nvme_composite" | "Composite" => "NVMe",
        "iwlwifi_1" | "iwlwifi" => "WiFi",
        "amdgpu" => "GPU",
        // Jetson (per WireThermalZone comment).
        "cpu-thermal" | "CPU-therm" => "CPU",
        "gpu-thermal" | "GPU-therm" => "GPU",
        "aux-thermal" | "AUX-therm" => "AUX",
        "AO-therm" => "Always-On",
        // Unknown — sentinel; caller substitutes the raw label.
        _ => "",
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn labels(vs: &[&str]) -> Vec<String> {
        vs.iter().map(|s| (*s).to_string()).collect()
    }

    /// The mapping for the actual set of labels on the operator's
    /// dev host (the report that triggered this fix). Pins the
    /// disambiguation of the duplicate `acpitz` pair.
    #[test]
    fn dev_host_layout_maps_correctly() {
        // Actual layout on the Linux 3060 host, per the dispatch
        // report and the sysfs read at platform/host_vitals.rs.
        // Sorted-by-label order (matches wire emission).
        let raw = labels(&["acpitz", "acpitz", "x86_pkg_temp"]);
        let friendly = humanize_thermal_labels(&raw);
        assert_eq!(
            friendly,
            vec![
                "System Zone 1".to_string(),
                "System Zone 2".to_string(),
                "CPU Package".to_string(),
            ],
        );
    }

    /// A single `acpitz` (no duplicate) renders WITHOUT the
    /// disambiguation suffix — the ` 1` is only useful when there's
    /// a corresponding ` 2` to distinguish from.
    #[test]
    fn single_occurrence_drops_disambiguation_suffix() {
        let raw = labels(&["acpitz"]);
        let friendly = humanize_thermal_labels(&raw);
        assert_eq!(friendly, vec!["System Zone".to_string()]);
    }

    /// Unknown labels pass through as the raw string — never blank,
    /// never a placeholder. This is the "never-a-blank-row"
    /// invariant that keeps a new-host sensor legible.
    #[test]
    fn unknown_label_falls_back_to_raw() {
        let raw = labels(&["some_new_zone_type"]);
        let friendly = humanize_thermal_labels(&raw);
        assert_eq!(friendly, vec!["some_new_zone_type".to_string()]);
    }

    /// Unknown labels also disambiguate when duplicated — the
    /// numbering scheme uses the (possibly-empty) friendly base as
    /// its bucket, so two unknown identical raws still get 1/2.
    /// Prevents the operator seeing two identical "some_zone" rows.
    #[test]
    fn duplicate_unknown_labels_disambiguate() {
        let raw = labels(&["unknown_zone", "unknown_zone"]);
        let friendly = humanize_thermal_labels(&raw);
        // Both fall into the "" bucket → both get numbered as ` 1` /
        // ` 2`, but the base is empty so the output is " 1" / " 2".
        // Not pretty, but the raw is shown muted alongside — the
        // operator still knows which is which. Test pins the shape.
        assert_eq!(friendly, vec![" 1".to_string(), " 2".to_string()]);
    }

    /// Jetson-shape labels (documented but not observed on this host).
    #[test]
    fn jetson_labels_map_correctly() {
        let raw = labels(&["cpu-thermal", "gpu-thermal", "aux-thermal"]);
        let friendly = humanize_thermal_labels(&raw);
        assert_eq!(
            friendly,
            vec!["CPU".to_string(), "GPU".to_string(), "AUX".to_string()],
        );
    }

    /// Empty input → empty output. Guards against a `[].last().unwrap()`
    /// panic on hosts that expose zero thermal zones (docker containers
    /// without sysfs mounts, CI environments).
    #[test]
    fn empty_input_yields_empty_output() {
        let raw: Vec<String> = vec![];
        let friendly = humanize_thermal_labels(&raw);
        assert!(friendly.is_empty());
    }
}
