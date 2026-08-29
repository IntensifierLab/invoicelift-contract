# Repayment waterfall threat model

## Contract scope

`repayment-waterfall` is intended to route buyer repayments through principal, fees, and lender distributions according to a deterministic priority order. The current scaffold exposes initialization, a protocol ping, and an ABI version marker.

## Trust assumptions

- Repayment sources are trusted only after asset, invoice, payer, and amount checks succeed.
- Pool share data supplied by the pool manager must be verified against canonical contract state.
- Fee recipients and lender recipients are configured by governance and should be authenticated addresses.
- Off-chain accounting systems are reconciliation aids, not sources of truth.

## Function call surface

| Function | Who can call today | What can go wrong | Mitigations |
| --- | --- | --- | --- |
| `initialize(env, admin)` | Any caller before initialization. | A front-runner can set an unintended scaffold admin symbol. | Initialize during deployment, retain the one-time guard, and upgrade to authenticated admin address checks before routing assets. |
| `ping(env, marker)` | Any caller. | Callers can confuse ping output with a successful repayment route. | Keep ping side-effect free and never use it for repayment status. |
| `version(env)` | Any caller. | Clients may assume version `1` means production waterfall math exists. | Document supported behavior and update this model when repayment functions are added. |

## Attack surfaces

- Repayments can be misrouted if invoice identifiers are not matched against registry and pool-manager state.
- Fee and principal ordering can be manipulated if the waterfall priority is not deterministic and tested.
- Rounding dust can accumulate or be stolen if split math is not explicit about residual handling.
- Re-entrant or repeated repayment calls can double-distribute funds if payment IDs are not idempotent.
- Unsupported assets can enter the route if token contract addresses are not allowlisted.

## Required mitigations for future repayment methods

- Authenticate or validate the repayment source, invoice ID, asset contract, and expected pool before distribution.
- Store processed repayment IDs to make distribution idempotent.
- Define a fixed priority order for principal, protocol fees, late fees, and lender allocations.
- Use deterministic integer math with explicit residual/dust policy and tests for boundary values.
- Emit events for repayment received, each distribution leg, residual handling, and rejected repayments.
- Add tests for duplicate repayments, unauthorized assets, rounding boundaries, and failed cross-contract lookups.