// Mirror of `ux_contract::limits::ACTIVITY_FEED_WEB_MAX` from
// `~/ux_contract/src/lib.rs` (v0.3.10 CAR-19c). The Rust build
// is the source of truth — the Sprint 6 wire bundle does not
// have a code-gen step for crate constants, so we mirror the
// value here by hand. A Rust-side regression test
// (`tests/web_limits_mirror.rs`) pins this file's number against
// the contract crate so the two cannot drift.
export const ACTIVITY_FEED_WEB_MAX = 12;
