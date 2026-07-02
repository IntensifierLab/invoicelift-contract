# Contract Issues Backlog

## Issue: Implement invoice-registry create/approve/assign
**Labels:** `core`, `soroban`
**Description:** Write the smart contract methods to handle the lifecycle of an invoice asset.
**Acceptance Criteria:**
- Only authorized parties can approve/verify an invoice.
- Ownership assignment securely transfers rights to the pool manager.

## Issue: Add concentration and exposure limits in pool-manager
**Labels:** `core`, `finance`
**Description:** Ensure that no single pool is over-exposed to a single buyer or SME to maintain healthy risk distribution.
**Acceptance Criteria:**
- Configurable limits per pool.
- Rejection of financing requests that breach limits.

