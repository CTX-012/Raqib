# Release process

Run from `~/edge_monitor-l14` on the release branch.

## 1. Bump version + changelog

- Edit `Cargo.toml` → bump `version = "X.Y.Z"`.
- Prepend a `## [X.Y.Z] — YYYY-MM-DD` block to `CHANGELOG.md` covering
  what landed since the previous tag.

## 2. Run all gates

```bash
cargo build --workspace
cargo build --release --workspace
cargo clippy --workspace --all-targets -- -D warnings
cargo test --workspace --all-targets
(cd web && npm run build)
```

All must be green before tagging.

## 3. Verify the release binary reports the bumped version

This step closes B-EMPIRICAL-3 from the v1.0.2 hotfix. The release
binary picks up the new `version` field from `Cargo.toml` only when
it is actually rebuilt — `cargo build` is a no-op when nothing under
`src/` changed, so a forgotten `--release` rebuild ships a binary
whose `--version` reports the old number.

```bash
# `cargo pkgid` emits either `edge_monitor@1.0.2` (Cargo ≥1.77) or
# the older `path+file:///…#1.0.2` form. The sed strips both shapes
# to the bare version string.
expected="$(cargo pkgid | sed -E 's/.*[#@]//')"   # → 1.0.2
reported="$(target/release/edge_monitor --version | awk '{print $NF}')"
test "$expected" = "$reported" \
  || { echo "release binary reports $reported, expected $expected"; exit 1; }
```

Run that snippet before tagging. If it fails, run
`cargo build --release --workspace` and re-check.

## 4. Commit, tag, push

```bash
git add Cargo.toml Cargo.lock CHANGELOG.md  # plus any other files in the release
git commit -m "release: vX.Y.Z"
git tag -a vX.Y.Z -m "vX.Y.Z — <one-line summary>"
git push origin <branch> vX.Y.Z
```

For hotfix releases (X.Y.Z where Z > 0) prefer the `fix: vX.Y.Z — …`
commit subject so the changelog reads cleanly.
