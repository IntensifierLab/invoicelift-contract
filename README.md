# InvoiceLift Contracts

Soroban workspace for the InvoiceLift protocol contract crates:

- `invoice-registry`: invoice lifecycle and verification scaffold.
- `pool-manager`: lender pool and exposure-limit scaffold.
- `repayment-waterfall`: repayment routing scaffold.

## Security documentation

Threat models are maintained per contract crate under `docs/`:

- [Invoice registry threat model](docs/invoice-registry-threat-model.md)
- [Pool manager threat model](docs/pool-manager-threat-model.md)
- [Repayment waterfall threat model](docs/repayment-waterfall-threat-model.md)

The current contracts expose scaffold methods (`initialize`, `ping`, and `version`). The threat models document who can call each method today, what can go wrong as domain logic is added, and the mitigations that should stay attached to each crate.