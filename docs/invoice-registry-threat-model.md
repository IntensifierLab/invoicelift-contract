# Invoice registry threat model

## Contract scope

`invoice-registry` is intended to own invoice lifecycle and verification state. The current scaffold exposes initialization, a protocol ping, and an ABI version marker. Future create, approve, verify, and assign flows should be measured against this model before they are merged.

## Trust assumptions

- The deployer or governance process chooses the initial admin symbol during `initialize`.
- Off-chain invoice documents, buyer identities, and verification evidence are trusted only after they are bound to deterministic on-chain identifiers.
- The pool manager is trusted to receive assignment rights only after invoice approval checks pass.
- Indexers and dashboards are observers; they must not be treated as authorization sources.

## Function call surface

| Function | Who can call today | What can go wrong | Mitigations |
| --- | --- | --- | --- |
| `initialize(env, admin)` | Any caller before the instance `admin` key exists. | A front-runner can initialize an uninitialized deployment with an unexpected admin symbol; repeated calls can try to overwrite governance. | Deploy through a controlled script, call initialization in the same release run, keep the existing one-time storage guard, and replace the scaffold symbol admin with authenticated admin address checks before production use. |
| `ping(env, marker)` | Any caller. | Integrators may mistake ping responses for invoice validity or liveness guarantees. | Keep ping side-effect free, do not attach business state to it, and document that it is only a connectivity marker. |
| `version(env)` | Any caller. | Consumers may assume a version number proves a complete invoice workflow. | Treat the number as an ABI/deployment marker only and pair it with documented release notes. |

## Attack surfaces

- Invoice creation can accept forged invoice metadata if signer, buyer, seller, amount, currency, and due-date checks are not bound to storage.
- Approval and verification flows can be abused if the verifier role is represented as a plain symbol instead of an authenticated address or role map.
- Assignment to a pool can transfer rights prematurely if invoice state transitions are not enforced in order.
- Duplicate invoice identifiers can enable double financing if uniqueness is not checked at creation time.
- Panic strings can leak inconsistent behavior to clients if typed errors are not introduced as domain logic grows.

## Required mitigations for future lifecycle methods

- Require authenticated caller addresses for admin, verifier, seller, buyer, and pool-manager actions.
- Store invoice state with explicit transitions such as `Created -> Verified -> Assigned -> Repaid` and reject skipped transitions.
- Hash or otherwise canonicalize off-chain invoice payloads before storing references.
- Enforce uniqueness for invoice IDs and assignment IDs.
- Emit events for creation, verification, assignment, rejection, and repayment handoff so auditors can reconstruct the lifecycle.
- Add tests for unauthorized callers, duplicate invoices, invalid transitions, and assignment before verification.