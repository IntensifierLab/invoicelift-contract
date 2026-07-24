# Attack Surface

Every externally callable entrypoint, its authorization, inputs, and state
effect. "Auth" = does the function verify the caller via `require_auth`?

## invoice-registry

| Function | Auth | Inputs | Effect |
| --- | --- | --- | --- |
| `initialize` | once-only guard | `admin: Address` | Sets the admin; errors if already set. |
| `transfer_admin` | ✅ current admin | `new_admin: Address` | Records a pending admin nomination. |
| `accept_admin` | ✅ pending admin | — | Promotes pending → admin; emits `AdminTransferred`. |
| `get_admin` / `get_pending_admin` | read-only | — | Returns admin state. |
| `ping` / `version` | none (pure) | `marker` | Utility; no state. |

> `invoice-registry` is de-scaffolded (issue #23): admin is an `Address`, the
> transfer is two-step, and every privileged call is `require_auth`-guarded.

## pool-manager

| Function | Auth | Inputs | Effect |
| --- | --- | --- | --- |
| `initialize` | once-only guard | `admin: Symbol`, `max_utilisation` | Seeds pool params. |
| `deposit` | ❌ **none** | `lender: Symbol`, `amount` | Mints shares to `lender`; updates totals. |
| `withdraw` | ❌ **none** | `lender: Symbol`, `shares` | Burns shares from `lender`; returns capital; clamps financed. |
| `finance` | ❌ **none** | `amount` | Increases financed amount under the utilisation cap. |
| `set_nav` | ❌ **none** | `new_nav` | Reprices the pool; re-derives capital; clamps financed. |
| `total_shares`, `total_capital`, `financed_amount`, `max_utilisation`, `nav`, `lender_shares` | read-only | — | Views. |

> **Critical:** none of the `pool-manager` mutators authenticate the caller, and
> `lender` is a free `Symbol` argument. Anyone can deposit/withdraw against any
> lender identity and call `finance` / `set_nav`. See
> [R-1](risks.md#r-1-missing-authorization-on-pool-manager-mutators) and
> [R-2](risks.md#r-2-caller-controlled-lender-identity).

### Arithmetic hot-spots (pool-manager)

- `shares = amount * NAV_SCALE / nav` (`deposit`) and
  `amount = shares * nav / NAV_SCALE` (`withdraw`) — unchecked `i128`
  multiplication; large inputs can overflow (panics in debug, wraps in release
  unless `overflow-checks` is set). See
  [R-3](risks.md#r-3-unchecked-arithmetic).
- `tot_capital * max_util / 10_000` (`finance`) — same overflow class.
- Integer division truncates; rounding direction should be reviewed for
  share-minting fairness.

## repayment-waterfall

| Function | Auth | Inputs | Effect |
| --- | --- | --- | --- |
| `initialize` | once-only guard | `admin: Symbol` | Sets the admin (scaffold). |
| `ping` / `version` | none (pure) | `marker` | Utility; no state. |

> `repayment-waterfall` is still a scaffold: the admin is an unenforced `Symbol`
> and no routing logic exists yet. See
> [R-4](risks.md#r-4-scaffold-contracts-not-production-ready).

## Cross-cutting

- **Panics as control flow.** The scaffold contracts use bare `panic!("…")`;
  `invoice-registry` uses a typed `#[contracterror]` enum. Auditors should
  confirm panics cannot be triggered to grief callers on read paths.
- **No reentrancy vector today** — no contract performs cross-contract calls
  into untrusted code before committing state. This must be re-checked if the
  waterfall begins invoking token contracts.
