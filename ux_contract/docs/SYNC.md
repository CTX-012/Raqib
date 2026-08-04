# ux_contract distribution

ux_contract is canonical at ~/ux_contract on the Linux side. The
Windows repo at <path> keeps a vendored copy at crates/ux_contract/.
Both repos consume their local copy via path dependency.

To sync: from this repo, run scripts/export-for-windows.sh /tmp/export,
then on the Windows side, copy /tmp/export contents into
crates/ux_contract/ (replacing existing files), verify
crates/ux_contract/SHA256SUMS matches, and commit. Sync runs after
every accepted amendment to the contract.
