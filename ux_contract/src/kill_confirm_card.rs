//! kill_confirm card — overlay shown after the user presses `k` to confirm
//! a kill on the focused workload.
//!
//! Mirrors the W23 `live_detail_card` / `post_mortem_card` overlay shape:
//! the card is centered, fixed-width per `sizing::CARD_WIDTH`, and lists
//! the workload's identifying fields above a yes/no prompt with an
//! `[Enter] / [Esc]` footer hint.
//!
//! Introduced in CAR-17 (v0.3.8). Replaces the top-of-screen ARMED banner
//! pattern used in earlier versions: kill is always real, and this card
//! IS the safety surface. The `kill_keybinding::ARMED_BANNER` constant
//! and `alerts::GOVERNOR_ARMED` template remain in the contract for
//! backward compatibility with v0.3.x consumers, and are scheduled for
//! removal in v0.4.x once Linux/Windows have migrated to the card.
//!
//! CAR-D82 (v0.3.19) — manual-k SIGKILL escalation states. D72 Position A:
//! the first `k`→Enter sends a *catchable* SIGTERM; if the PID survives the
//! operator-configured grace window (`sigterm_grace_secs`), the card flips
//! from the Confirm state to a Waiting state that offers an explicit
//! force-SIGKILL. This fixes the live "k didn't kill ollama" symptom — a
//! SIGTERM-ignoring daemon survives the graceful signal, and today's
//! manual path has no escalation. The Confirm → Waiting → ForceConfirm
//! state machine is CONSUMER-side; this module only owns the strings the
//! Waiting/ForceConfirm states render. The escalation REUSES the D81
//! auto-kill machinery (`execute_after_grace` + `send_sigkill`,
//! identity-guarded) — no new kill machinery is introduced here or by the
//! consumer. The SIGKILL footer-result counterpart to the graceful
//! `status::KILL_SENT` lives beside it in `status` (`status::KILL_FORCE_SENT`),
//! not here, because it is a footer status message, not a card-internal label.

/// Title rendered at the top of the kill_confirm card.
pub const KILL_CONFIRM_TITLE: &str = "Kill Confirmation";

/// Prompt rendered above the field list. Phrased as a question so the
/// user explicitly answers yes/no rather than acknowledging a statement.
pub const KILL_CONFIRM_PROMPT: &str = "Kill this process?";

/// Footer hint rendered at the bottom of the card. Lists both exits
/// (confirm and cancel) so users always see how to leave the card.
pub const KILL_CONFIRM_HINT: &str = "[Enter] confirm  ·  [Esc] cancel";

/// "PID:" — process-id field label.
pub const KILL_CONFIRM_PID_LABEL: &str = "PID:";

/// "Workload:" — workload-name field label.
pub const KILL_CONFIRM_WORKLOAD_LABEL: &str = "Workload:";

/// "Category:" — workload category field label (LLM / Vision / etc., see
/// `workload_category::*`).
pub const KILL_CONFIRM_CATEGORY_LABEL: &str = "Category:";

/// "Status:" — current `WorkloadStatus` field label.
pub const KILL_CONFIRM_STATUS_LABEL: &str = "Status:";

/// "Running for:" — wall-clock runtime field label.
pub const KILL_CONFIRM_RUNTIME_LABEL: &str = "Running for:";

/// "RAM:" — current resident-memory usage field label.
pub const KILL_CONFIRM_RAM_LABEL: &str = "RAM:";

/// "VRAM:" — current VRAM usage field label.
pub const KILL_CONFIRM_VRAM_LABEL: &str = "VRAM:";

/// "CPU:" — current CPU usage field label.
pub const KILL_CONFIRM_CPU_LABEL: &str = "CPU:";

// ----------------------------------------------------------------------------
// CAR-D82 (v0.3.19) — manual-k SIGKILL escalation states
// ----------------------------------------------------------------------------
//
// Rendered by the card's Waiting / ForceConfirm states (see module docs).
// The Confirm-state strings above are unchanged; these are additive and the
// pre-D82 single-state card keeps working untouched.

/// Prompt rendered in the card's WAITING state — shown after the operator's
/// first Enter has sent SIGTERM, while the card holds open for the grace
/// window before offering escalation. Phrased to make two things explicit so
/// the operator is not left wondering whether anything happened: (a) the
/// graceful signal (SIGTERM) has ALREADY been sent, and (b) how long the card
/// will wait before escalation is offered.
///
/// `{secs}` is the integer seconds remaining in the grace window, counting
/// down — the SAME `{secs}` render-time placeholder convention used by
/// [`crate::status::KILL_ARMED`] and [`crate::status::KILL_COUNTDOWN`]. The
/// value is the consumer's operator-configured `sigterm_grace_secs`,
/// substituted at render time; the contract does NOT carry a grace-duration
/// constant (it is operator config on the consumer side, not a UX default).
pub const KILL_CONFIRM_WAITING_PROMPT: &str =
    "SIGTERM sent — waiting {secs}s for graceful shutdown…";

/// Footer hint rendered in the WAITING state, replacing
/// [`KILL_CONFIRM_HINT`] while the card waits. Lists both exits the operator
/// has during the grace window: Enter escalates to a force-kill, Esc cancels
/// (the SIGTERM already sent is not recalled — Esc just dismisses the card and
/// stops the escalation). Mirrors the `[Enter] … · [Esc] …` shape and the
/// two-space-middot spacing of [`KILL_CONFIRM_HINT`] so the footer is visually
/// stable across card states. The verb is the lowercase key-hint form;
/// [`KILL_CONFIRM_FORCE_SIGKILL`] is the formal action label naming the signal.
pub const KILL_CONFIRM_WAITING_HINT: &str = "[Enter] force-kill  ·  [Esc] cancel";

/// Formal action label for the force-SIGKILL escalation — the action the
/// operator consents to by pressing Enter in the Waiting state. Names the
/// signal (SIGKILL) explicitly so the consent is INFORMED: this is the
/// uncatchable kill, distinct from the catchable SIGTERM sent first. Use this
/// where the card needs the full action name (e.g. a ForceConfirm-state
/// prompt / button), as opposed to the compact footer verb in
/// [`KILL_CONFIRM_WAITING_HINT`].
pub const KILL_CONFIRM_FORCE_SIGKILL: &str = "Force-kill (SIGKILL)";
