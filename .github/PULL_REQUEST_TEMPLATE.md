## What
<!-- one paragraph: what this PR does and why -->

## Spec traceability delta
<!-- which spec statements this PR implements/changes; update spec/traceability.md
     in the same PR (rule: PR description IS the review artifact) -->

## Test evidence
- `cargo test --workspace`: N passed / 0 failed
- `cargo clippy --workspace --all-targets -- -D warnings`: clean
- coverage (if relevant): `cargo llvm-cov --summary-only` → X%

## Data safety
<!-- leak scan run on the diff: no personal data, no real session content,
     no profile names, no machine paths beyond scheme-level ~/.hermes -->
