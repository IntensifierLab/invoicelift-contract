#!/usr/bin/env bash
#
# scripts/deploy-testnet.sh
#
# Builds and deploys the three InvoiceLift Soroban contracts to a Stellar
# network, in dependency order:
#
#   1. pool-manager
#   2. invoice-registry
#   3. repayment-waterfall
#
# Each contract is built to wasm, deployed, and initialized. The resulting
# contract IDs are printed and written to `.env.testnet` at the repo root.
#
# Usage:
#   scripts/deploy-testnet.sh [--network <network>] [--source-account <identity>]
#
# Environment variables (all optional):
#   NETWORK               Same as --network            (default: testnet)
#   DEPLOYER_IDENTITY     Same as --source-account      (default: deployer)
#   ADMIN_SYMBOL           Admin identifier passed to pool-manager and
#                          repayment-waterfall's `initialize` (both take a
#                          Symbol, not an Address)        (default: admin)
#   MAX_UTILISATION_BPS   Max pool utilisation passed to pool-manager's
#                          `initialize`, in basis points   (default: 8000)
#
# Requires the Stellar CLI (`stellar`, or the older `soroban` alias) and a
# Rust toolchain with the wasm32-unknown-unknown target installed.

set -euo pipefail

SCRIPT_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
REPO_ROOT="$(cd "${SCRIPT_DIR}/.." && pwd)"

NETWORK="${NETWORK:-testnet}"
SOURCE_ACCOUNT="${DEPLOYER_IDENTITY:-deployer}"
ADMIN_SYMBOL="${ADMIN_SYMBOL:-admin}"
MAX_UTILISATION="${MAX_UTILISATION_BPS:-8000}"
ENV_FILE="${REPO_ROOT}/.env.testnet"

usage() {
  cat <<EOF
Usage: $(basename "$0") [--network <network>] [--source-account <identity>]

Deploys pool-manager, invoice-registry and repayment-waterfall (in that
order) to a Stellar/Soroban network and writes their contract IDs to:
  ${ENV_FILE}

Options:
  --network <network>          Network to deploy to (default: ${NETWORK})
  --source-account <identity>  CLI identity used to sign transactions
                                (default: ${SOURCE_ACCOUNT})
  -h, --help                   Show this help text

See the top of this script for supported environment variables.
EOF
}

while [[ $# -gt 0 ]]; do
  case "$1" in
    --network)
      NETWORK="$2"
      shift 2
      ;;
    --network=*)
      NETWORK="${1#*=}"
      shift
      ;;
    --source-account)
      SOURCE_ACCOUNT="$2"
      shift 2
      ;;
    --source-account=*)
      SOURCE_ACCOUNT="${1#*=}"
      shift
      ;;
    -h|--help)
      usage
      exit 0
      ;;
    *)
      echo "error: unknown argument: $1" >&2
      usage >&2
      exit 1
      ;;
  esac
done

# ---------------------------------------------------------------------------
# Preflight checks
# ---------------------------------------------------------------------------

if command -v stellar >/dev/null 2>&1; then
  CLI=stellar
elif command -v soroban >/dev/null 2>&1; then
  CLI=soroban
else
  echo "error: neither 'stellar' nor 'soroban' CLI found on PATH." >&2
  echo "       Install the Stellar CLI: https://developers.stellar.org/docs/tools/cli/install-cli" >&2
  exit 1
fi

if ! command -v cargo >/dev/null 2>&1; then
  echo "error: 'cargo' not found on PATH. Install the Rust toolchain first." >&2
  exit 1
fi

echo "==> CLI:            ${CLI}"
echo "==> network:        ${NETWORK}"
echo "==> source account: ${SOURCE_ACCOUNT}"

# Make sure the signing identity exists locally, generating (and funding via
# friendbot) it if it doesn't. This only makes sense on testnet/futurenet.
if ! "${CLI}" keys address "${SOURCE_ACCOUNT}" >/dev/null 2>&1; then
  echo "==> identity '${SOURCE_ACCOUNT}' not found locally, generating and funding it"
  "${CLI}" keys generate "${SOURCE_ACCOUNT}" --network "${NETWORK}" --fund
fi

ADMIN_ADDRESS="$("${CLI}" keys address "${SOURCE_ACCOUNT}")"
echo "==> admin address:  ${ADMIN_ADDRESS}"

# ---------------------------------------------------------------------------
# Build
# ---------------------------------------------------------------------------

WASM_DIR="${REPO_ROOT}/target/wasm32-unknown-unknown/release"

build_crate() {
  local crate="$1"
  echo "==> building ${crate} (release, wasm32-unknown-unknown)"
  (cd "${REPO_ROOT}" && cargo build --release --target wasm32-unknown-unknown -p "${crate}")
}

build_crate pool-manager
build_crate invoice-registry
build_crate repayment-waterfall

POOL_MANAGER_WASM="${WASM_DIR}/pool_manager.wasm"
INVOICE_REGISTRY_WASM="${WASM_DIR}/invoice_registry.wasm"
REPAYMENT_WATERFALL_WASM="${WASM_DIR}/repayment_waterfall.wasm"

for f in "${POOL_MANAGER_WASM}" "${INVOICE_REGISTRY_WASM}" "${REPAYMENT_WATERFALL_WASM}"; do
  if [[ ! -f "${f}" ]]; then
    echo "error: expected wasm artifact not found: ${f}" >&2
    exit 1
  fi
done

# ---------------------------------------------------------------------------
# Deploy helpers
# ---------------------------------------------------------------------------

deploy_contract() {
  local wasm="$1"
  "${CLI}" contract deploy \
    --wasm "${wasm}" \
    --source-account "${SOURCE_ACCOUNT}" \
    --network "${NETWORK}" \
    | tail -n 1
}

# ---------------------------------------------------------------------------
# 1. pool-manager
# ---------------------------------------------------------------------------

echo ""
echo "==> [1/3] deploying pool-manager"
POOL_MANAGER_ID="$(deploy_contract "${POOL_MANAGER_WASM}")"
echo "    pool-manager contract ID: ${POOL_MANAGER_ID}"

echo "==> [1/3] initializing pool-manager (admin=${ADMIN_SYMBOL}, max_utilisation=${MAX_UTILISATION})"
"${CLI}" contract invoke \
  --id "${POOL_MANAGER_ID}" \
  --source-account "${SOURCE_ACCOUNT}" \
  --network "${NETWORK}" \
  -- initialize \
  --admin "${ADMIN_SYMBOL}" \
  --max_utilisation "${MAX_UTILISATION}"

# ---------------------------------------------------------------------------
# 2. invoice-registry
# ---------------------------------------------------------------------------

echo ""
echo "==> [2/3] deploying invoice-registry"
INVOICE_REGISTRY_ID="$(deploy_contract "${INVOICE_REGISTRY_WASM}")"
echo "    invoice-registry contract ID: ${INVOICE_REGISTRY_ID}"

echo "==> [2/3] initializing invoice-registry (admin=${ADMIN_ADDRESS})"
"${CLI}" contract invoke \
  --id "${INVOICE_REGISTRY_ID}" \
  --source-account "${SOURCE_ACCOUNT}" \
  --network "${NETWORK}" \
  -- initialize \
  --admin "${ADMIN_ADDRESS}"

# ---------------------------------------------------------------------------
# 3. repayment-waterfall
# ---------------------------------------------------------------------------

echo ""
echo "==> [3/3] deploying repayment-waterfall"
REPAYMENT_WATERFALL_ID="$(deploy_contract "${REPAYMENT_WATERFALL_WASM}")"
echo "    repayment-waterfall contract ID: ${REPAYMENT_WATERFALL_ID}"

# NOTE: repayment-waterfall's `initialize` currently only takes an admin
# Symbol and does not yet reference pool-manager or invoice-registry.
# POOL_MANAGER_ID and INVOICE_REGISTRY_ID are already in scope by this point
# (deployed first, in dependency order) so that once repayment-waterfall
# grows a dependency on either contract's address, this is the one place to
# thread it through as an extra --<arg> to the `invoke` call below.
echo "==> [3/3] initializing repayment-waterfall (admin=${ADMIN_SYMBOL})"
"${CLI}" contract invoke \
  --id "${REPAYMENT_WATERFALL_ID}" \
  --source-account "${SOURCE_ACCOUNT}" \
  --network "${NETWORK}" \
  -- initialize \
  --admin "${ADMIN_SYMBOL}"

# ---------------------------------------------------------------------------
# Record results
# ---------------------------------------------------------------------------

cat > "${ENV_FILE}" <<EOF
# Generated by scripts/deploy-testnet.sh on $(date -u +"%Y-%m-%dT%H:%M:%SZ")
# Network: ${NETWORK}
NETWORK=${NETWORK}
POOL_MANAGER_CONTRACT_ID=${POOL_MANAGER_ID}
INVOICE_REGISTRY_CONTRACT_ID=${INVOICE_REGISTRY_ID}
REPAYMENT_WATERFALL_CONTRACT_ID=${REPAYMENT_WATERFALL_ID}
EOF

echo ""
echo "==> deployment complete, contract IDs written to ${ENV_FILE}"
echo "    POOL_MANAGER_CONTRACT_ID=${POOL_MANAGER_ID}"
echo "    INVOICE_REGISTRY_CONTRACT_ID=${INVOICE_REGISTRY_ID}"
echo "    REPAYMENT_WATERFALL_CONTRACT_ID=${REPAYMENT_WATERFALL_ID}"
