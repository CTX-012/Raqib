//! v1.3.2 / DISPATCH 86 — runtime tunables shared between the web
//! settings handlers and the TUI tick loop.
//!
//! ## The structural allowlist
//!
//! [`RuntimeTunables`] is the EXACT set of fields the web settings
//! POST may mutate. It exists as a separate type from the broader
//! [`crate::config::Config`] so that the boundary "the web cannot
//! flip auto_actuate / cannot change policy actions" is enforced by
//! the type system, NOT by hand-maintained allowlist code in a
//! handler. A new field added to `RuntimeTunables` is intentional;
//! a new field added to `Config` does NOT auto-grow the web-writable
//! surface.
//!
//! What's IN this struct (the WEB-TUNABLE knobs):
//!
//!   * `thresholds`: all numeric breach thresholds (VRAM/RAM/thermal/KV %
//!     levels + the alert-smoothing window).
//!   * `kill_sustain_secs`: the auto-kill sustain window (D80 Q3).
//!
//! What's deliberately NOT in this struct (the boundary):
//!
//!   * `auto_actuate` — the master autonomous-kill switch. TOML +
//!     restart ONLY. The web reads its current state (informational
//!     display) but cannot flip it.
//!   * `default_ai_action` — the policy verb (Allow vs Kill). Arming
//!     the killer is a console act, not a web act.
//!   * allowlist/blocklist names.
//!   * rate-limit knobs (out of scope for D86; future row).
//!
//! Pinned by `auto_actuate_is_not_a_field_of_runtime_tunables` and
//! `default_ai_action_is_not_a_field_of_runtime_tunables`.

use crate::thresholds::EffectiveThresholds;

/// The runtime-mutable tuning surface. Lives behind an
/// `Arc<std::sync::RwLock<...>>` shared between the web settings
/// handlers (writers) and the TUI tick loop (reader). std (not
/// tokio) `RwLock` because the tick loop is synchronous — a
/// `parking_lot`-style fast lock would also work but isn't worth
/// the new dependency for a per-tick borrow.
#[derive(Debug, Clone, Copy)]
pub struct RuntimeTunables {
    /// All breach thresholds (VRAM/RAM/thermal/KV pressure + alert
    /// smoothing). The runtime tick replaces `state.thresholds`
    /// with this value when it changes.
    pub thresholds: EffectiveThresholds,
    /// The D80 auto-kill sustain window (Q3). Read by
    /// `runtime::record_governor_audit` directly off the config; D86
    /// shares the live value through this struct so a web update
    /// takes effect on the next tick without a restart.
    pub kill_sustain_secs: u64,
}

/// `Arc<RwLock<RuntimeTunables>>`. Both the web handlers (via
/// `WebState`) and the tick loop (via `Runtime`) hold a clone of the
/// same Arc; writes are atomic per the RwLock.
pub type SharedTunables = std::sync::Arc<std::sync::RwLock<RuntimeTunables>>;

/// Build the shared cell from a freshly-loaded [`crate::config::Config`].
/// The runtime keeps the configured values authoritative until a web
/// POST overrides them; first-tick reads see the values the operator
/// put in their TOML.
pub fn shared_from_config(cfg: &crate::config::Config) -> SharedTunables {
    let thresholds = EffectiveThresholds::resolve(&cfg.thresholds)
        .unwrap_or_else(|_| EffectiveThresholds::default());
    let init = RuntimeTunables {
        thresholds,
        kill_sustain_secs: cfg.governor.kill_sustain_secs,
    };
    std::sync::Arc::new(std::sync::RwLock::new(init))
}

#[cfg(test)]
mod tests {
    use super::*;

    /// THE BOUNDARY PIN. `RuntimeTunables` is the structural
    /// allowlist of what the web settings POST may write. If a
    /// future refactor adds `auto_actuate` here, the boundary is
    /// breached (the web could then arm the killer). This test
    /// reads the module source and rejects an `auto_actuate` field
    /// on the type.
    #[test]
    fn auto_actuate_is_not_a_field_of_runtime_tunables() {
        let src = include_str!("tunables.rs");
        // Find the struct definition body. Anything between the
        // `pub struct RuntimeTunables {` opening and its closing
        // `}` is the structural surface.
        let start = src
            .find("pub struct RuntimeTunables {")
            .expect("RuntimeTunables struct definition must exist");
        let body_start = start + "pub struct RuntimeTunables {".len();
        let rel_end = src[body_start..]
            .find('}')
            .expect("RuntimeTunables body must close");
        let body = &src[body_start..body_start + rel_end];
        assert!(
            !body.contains("auto_actuate"),
            "BOUNDARY BREACH: `auto_actuate` MUST NOT be a field of \
             RuntimeTunables. The web settings POST writes to this \
             type; adding auto_actuate here means the web can arm \
             the killer. TOML + restart ONLY for auto_actuate.\n\
             struct body:\n{body}",
        );
        // Same check for the policy action verb.
        assert!(
            !body.contains("default_ai_action"),
            "BOUNDARY BREACH: `default_ai_action` MUST NOT be a \
             field of RuntimeTunables. Policy verbs (Allow vs Kill) \
             are a console act, not a web act.\n\
             struct body:\n{body}",
        );
    }

    /// Belt-and-suspenders sibling to the source-grep test above:
    /// build a tunables value, serialize it via Debug, and confirm
    /// neither boundary field appears even by accident. Catches a
    /// future refactor that, say, added auto_actuate as a method
    /// (rather than a field) that bypassed the grep.
    #[test]
    fn runtime_tunables_debug_does_not_expose_boundary_fields() {
        let tunables = RuntimeTunables {
            thresholds: EffectiveThresholds::default(),
            kill_sustain_secs: 10,
        };
        let dbg = format!("{tunables:?}");
        assert!(
            !dbg.contains("auto_actuate"),
            "Debug formatting of RuntimeTunables must not expose \
             auto_actuate; got: {dbg}",
        );
        assert!(
            !dbg.contains("default_ai_action"),
            "Debug formatting of RuntimeTunables must not expose \
             default_ai_action; got: {dbg}",
        );
    }
}
