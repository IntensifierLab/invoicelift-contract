# Known Risks & Mitigations

Ranked highest-impact first. Each risk has a status: **Open**, **Mitigated**, or
**Accepted**. This list is deliberately honest — it is the starting point for the
audit, not a claim of completeness.

## R-1: Missing authorization on pool-manager mutators

- **Severity:** Critical
- **Status:** Open
- **Where:** `pool-manager` — `deposit`, `withdraw`, `finance`, `set_nav`.
- **Description:** None of these state-mutating entrypoints call `require_auth`.
  Any account can reprice the pool (`set_nav`), consume financing capacity
  (`finance`), or move shares. The stored `admin` is a `Symbol` and is never
  checked.
- **Mitigation:** Migrate `admin` to `Address`, add `admin.require_auth()` to
  `finance` and `set_nav`, and require the lender's own auth on `deposit` /
  `withdraw`. This mirrors the `invoice-registry` de-scaffolding in issue #23;
  apply the same pattern here before mainnet.

## R-2: Caller-controlled lender identity

- **Severity:** High
- **Status:** Open
- **Where:** `pool-manager` — `deposit(lender: Symbol, …)`, `withdraw`.
- **Description:** `lender` is a free-form `Symbol` argument rather than an
  authenticated `Address`. Positions are keyed by an unauthenticated label, so
  callers can credit or debit arbitrary lender identities.
- **Mitigation:** Key `LenderPosition` by `Address` and derive it from
  `require_auth`, not from an argument.

## R-3: Unchecked arithmetic

- **Severity:** Medium
- **Status:** Open
- **Where:** `pool-manager` — share/capital/utilisation math.
- **Description:** `i128` multiplications (`amount * NAV_SCALE`,
  `tot_capital * max_util`) are unchecked. In release builds Rust wraps on
  overflow unless `overflow-checks = true`; a wrap here corrupts accounting.
  Division truncation may also bias share minting.
- **Mitigation:** Use `checked_mul` / `checked_div` and reject on `None`, or set
  `overflow-checks = true` for release, and document rounding direction.

## R-4: Scaffold contracts not production-ready

- **Severity:** Medium
- **Status:** Open
- **Where:** `repayment-waterfall` (and, before #23, `invoice-registry`).
- **Description:** `repayment-waterfall` still has the `initialize` scaffold with
  an unenforced `Symbol` admin and no domain logic. Shipping it as-is would
  deploy an unprotected contract.
- **Mitigation:** De-scaffold using the `invoice-registry` pattern (issue #23)
  before the waterfall is given real routing responsibilities. Tracked here so
  it is not left as a silent `// TODO`.

## R-5: Panic-based error handling on scaffolds

- **Severity:** Low
- **Status:** Partially mitigated
- **Where:** `pool-manager`, `repayment-waterfall` use bare `panic!`.
- **Description:** Bare panics produce opaque host traps and are harder for
  integrators to handle than typed errors.
- **Mitigation:** Adopt the `#[contracterror]` pattern already used in
  `invoice-registry` (`Error` enum) across all contracts.

## Dependency advisories

- **Severity:** Informational
- **Status:** Mitigated
- **Description:** `cargo audit` (issue #10) reports advisories in the
  host/`testutils`/build dependency tree (`curve25519-dalek`, `time`, `adler`,
  `paste`, `rand`). **None is reachable from the deployed `#![no_std]` contract
  Wasm**, which links `soroban-sdk` without `testutils`.
- **Mitigation:** Each is suppressed with a written justification in
  `.cargo/audit.toml`; any *new*, unlisted advisory fails CI. Revisit whenever
  dependencies change.

## Wasm size

- **Severity:** Informational
- **Status:** Mitigated
- **Description:** Oversized contracts fail deployment. Current sizes are far
  under the 64KB gate (registry ~2KB, pool-manager ~16KB, waterfall ~2KB).
- **Mitigation:** CI fails any crate exceeding 64KB (issue #8) and records sizes
  as an artefact.
