# Audit Scope

## In scope

The three workspace contract crates and their public entrypoints:

- `invoice-registry/src/lib.rs`
- `pool-manager/src/lib.rs`
- `repayment-waterfall/src/lib.rs`

Target platform: **Stellar Soroban**, `soroban-sdk = 20.5.0`, Rust edition 2021,
`rust-version = 1.78`, compiled to `wasm32-unknown-unknown`.

## Out of scope

- `ethnum-1.5.0/` — a vendored, patched copy of a third-party crate
  (`[patch.crates-io]`). It is a dependency, not project code; review is limited
  to *whether the patch is warranted*, not the crate's internals.
- Off-chain components: deployment scripts, front-ends, indexers, key
  management, and the Stellar network/host itself.
- The `soroban-sdk` and Soroban host — assumed correct and in the trusted
  computing base.

## Build & commit

| Field | Value |
| --- | --- |
| Repository | `IntensifierLab/invoicelift-contract` |
| Branch | `main` |
| Toolchain | `stable` (CI) with `wasm32-unknown-unknown` target |
| Reproduce | `cargo build --release --target wasm32-unknown-unknown --workspace` |

CI enforces `cargo check --workspace`, a per-crate **Wasm size gate** (≤ 64KB,
issue #8), and **`cargo audit`** dependency scanning (issue #10).

## Trust model

- **Admin.** Each contract has a single administrator set once at `initialize`.
  In `invoice-registry` the admin is an `Address` and privileged calls require
  `require_auth` (two-step transfer, issue #23). In `pool-manager` and
  `repayment-waterfall` the admin is currently a `Symbol` **without enforcement**
  — see [risks.md](risks.md#r-1-missing-authorization-on-pool-manager-mutators).
- **Lenders / users.** Untrusted. They may call any public entrypoint with any
  arguments; the contract must uphold its invariants regardless.
- **Value at risk.** `pool-manager` tracks share ownership and pool capital;
  incorrect accounting or missing authorization there is the highest-impact
  class of bug.

## Key invariants to verify

1. **Pool accounting identity:** `total_capital == total_shares * nav / NAV_SCALE`
   after every `deposit`, `withdraw`, and `set_nav`.
2. **Utilisation cap:** `financed_amount <= total_capital * max_utilisation / 10_000`
   holds after `finance`, `withdraw`, and `set_nav` (the last two *clamp* it).
3. **Share conservation:** a lender can never withdraw more shares than they hold;
   `total_shares` equals the sum of all `LenderPosition.shares`.
4. **Initialization is once-only** for every contract.

## Test coverage

- `pool-manager` ships formal invariant tests for the accounting identity and
  utilisation cap (`pool-manager/src/lib.rs`, `mod tests`).
- Coverage is measured with `cargo llvm-cov`:

  ```bash
  cargo install cargo-llvm-cov --locked
  cargo llvm-cov --workspace --summary-only
  ```

- **Current gaps** are the authorization branches on the not-yet-de-scaffolded
  contracts (`pool-manager`, `repayment-waterfall`). These become reachable and
  testable once those admins move to `Address` + `require_auth`, mirroring the
  `invoice-registry` work in issue #23. Until then the branches are documented
  as risks rather than papered over with tests that assert insecure behaviour.
