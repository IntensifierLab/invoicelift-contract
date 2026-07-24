# Security Audit Preparation

> Tracking issue: **#34** — prepare all three contracts for professional audit.

This directory is the audit-readiness package for the InvoiceLift Soroban
contracts. It gives an external auditor everything needed to scope and begin a
review without first reverse-engineering the codebase.

## Contents

| Document | Purpose |
| --- | --- |
| [scope.md](scope.md) | **Audit scope** — what is in and out of scope, commit, build, and the trust model. |
| [attack-surface.md](attack-surface.md) | **Attack surface** — every externally callable entrypoint, its authorization, inputs, and state effects. |
| [risks.md](risks.md) | **Known risks & mitigations** — the issues we already know about, ranked, each with a mitigation or status. |

## Contracts under review

| Crate | Role | LoC (src) |
| --- | --- | --- |
| `invoice-registry` | Invoice lifecycle, verification, admin | small |
| `pool-manager` | Lender pools, shares/NAV accounting, utilisation limits | ~560 |
| `repayment-waterfall` | Priority repayment routing | small |

## Readiness checklist (issue #34 acceptance)

- [x] **Attack surface document** — [attack-surface.md](attack-surface.md).
- [x] **Known risk list with mitigations** — [risks.md](risks.md).
- [x] **Audit scope document** — [scope.md](scope.md).
- [x] **All TODOs resolved / tracked** — the only source TODOs were the
  `initialize` "replace with auth in production" scaffolds. `invoice-registry`
  is de-scaffolded to a real `Address`-based, two-step admin (issue #23);
  the remaining scaffolds are catalogued as open risks in
  [risks.md](risks.md) rather than left as silent `// TODO`s.
- [x] **cargo-audit clean** — dependency scanning runs in CI with a justified
  suppression list (issue #10); see [risks.md](risks.md#dependency-advisories).
- [~] **100% branch test coverage** — coverage measurement and the current
  gap-by-gap status are documented in [scope.md](scope.md#test-coverage). The
  accounting invariants in `pool-manager` already have formal invariant tests;
  the auth-path branches are covered as each contract is de-scaffolded.

## How to reproduce the review environment

```bash
# Build all contracts (native + Wasm)
cargo check --workspace
cargo build --release --target wasm32-unknown-unknown --workspace

# Run the test suite
cargo test --workspace

# Dependency advisories (see .cargo/audit.toml for justified suppressions)
cargo install cargo-audit --locked
cargo audit --deny warnings
```
