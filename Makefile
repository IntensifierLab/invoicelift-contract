.PHONY: deploy-testnet

# Build and deploy pool-manager, invoice-registry and repayment-waterfall (in
# that dependency order) to Stellar testnet. Contract IDs are printed and
# written to .env.testnet. See scripts/deploy-testnet.sh for options/env vars.
deploy-testnet:
	./scripts/deploy-testnet.sh
