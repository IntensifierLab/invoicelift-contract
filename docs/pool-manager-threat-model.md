# Pool manager threat model

## Contract scope

`pool-manager` is intended to manage lender pools, pool limits, and financing exposure constraints. The current scaffold exposes initialization, a protocol ping, and an ABI version marker.

## Trust assumptions

- Governance controls pool creation, pause, exposure-limit updates, and emergency changes.
- Lender deposits and withdrawals will eventually rely on authenticated Stellar addresses, not symbolic placeholders.
- Invoice exposure data received from `invoice-registry` is trusted only when it is passed through verified contract calls or validated events.
- Off-chain dashboards can recommend pool settings, but the contract must enforce final limits.

## Function call surface

| Function | Who can call today | What can go wrong | Mitigations |
| --- | --- | --- | --- |
| `initialize(env, admin)` | Any caller before initialization. | An untrusted actor can claim the scaffold admin slot on a fresh deployment. | Initialize immediately after deployment, keep the one-time guard, and replace symbol-based admin storage with authenticated governance before funds are managed. |
| `ping(env, marker)` | Any caller. | Callers may treat ping as proof that pool limits or solvency checks are active. | Keep ping read-only and document it as a connectivity check only. |
| `version(env)` | Any caller. | Integrators may map a version to unsupported financial behavior. | Tie version bumps to release notes and threat-model updates. |

## Attack surfaces

- Pool creation can be abused if admins are not authenticated and if duplicate pool identifiers are accepted.
- Exposure-limit updates can become an admin rug vector if changes are immediate and unaudited.
- Financing requests can over-concentrate risk if buyer, seller, maturity, and pool-level caps are not checked atomically.
- Deposits and withdrawals can be mis-accounted if share math rounds in favor of the protocol or caller.
- Cross-contract calls to invoice registry can become stale if invoice status is cached without verification.

## Required mitigations for future pool methods

- Require authenticated governance for pool creation, limit changes, pause, and emergency actions.
- Add time delay or event-heavy auditability for material risk-limit changes.
- Check pool capacity, per-buyer exposure, per-seller exposure, maturity limits, and invoice assignment in a single transaction path.
- Use integer fixed-point math with tests for rounding, dust, and maximum-value inputs.
- Emit events for pool creation, limit changes, financing approvals/rejections, deposits, withdrawals, and pauses.
- Add tests for unauthorized admin actions, duplicate pool IDs, over-limit financing, and stale invoice references.