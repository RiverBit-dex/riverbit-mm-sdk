package main

import (
	"context"
	"errors"
	"fmt"
	"io"
	"log/slog"
	"os"
	"strconv"
	"time"

	"github.com/riverbit-dex/riverbit-mm-sdk/go/exchange"
)

// runQuota prints the request quota of the trading account. Without
// BUY_REQUESTS it only reads (unsigned /info queries) and sends nothing signed.
// With BUY_REQUESTS=<n> it also buys n requests with a reserveRequestWeight
// action, prints the receipt and prints the quota again.
//
// The purchase is paid by the signer's account (the master for an API wallet),
// or by the vault when GUILD_VAULT_ADDRESS is set. The SDK never buys on its own.
func runQuota(ctx context.Context, cfg Config, logger *slog.Logger, out io.Writer) error {
	var buy uint64
	if cfg.BuyRequests != "" {
		n, err := strconv.ParseUint(cfg.BuyRequests, 10, 64)
		if err != nil || n < 1 || n > exchange.MaxReserveWeight {
			return fmt.Errorf("BUY_REQUESTS must be an integer from 1 to %d", exchange.MaxReserveWeight)
		}
		buy = n
	}
	client, err := newClient(cfg, cfg.GuildVault)
	if err != nil {
		return err
	}
	client.StateDir = cfg.StateDir
	client.Logger = logger
	if err = resolveAccountREST(ctx, client, cfg, logger); err != nil {
		return err
	}
	account := client.AccountAddress()
	before, err := printQuota(ctx, client, out, "before")
	if err != nil {
		return err
	}
	if buy == 0 {
		fmt.Fprintln(out, "Nothing was sent. Set BUY_REQUESTS=<n> to buy n requests (0.0005 USDC each, paid by the account above).")
		return nil
	}

	action := exchange.NewReserveRequestWeight(buy)
	fmt.Fprintf(out, "Buying %d requests for %d micro-USDC (paid by %s)\n", buy, action.CostMicroUSDC(), account)
	if err = client.LoadChainID(ctx); err != nil {
		return err
	}
	if cfg.GuildVault != "" {
		if err = client.VerifySubAccount(ctx); err != nil {
			return fmt.Errorf("verify vault signer authorization: %w", err)
		}
	}
	recoverCtx, cancel := context.WithTimeout(ctx, 30*time.Second)
	err = client.RecoverPending(recoverCtx)
	cancel()
	if errors.Is(err, exchange.ErrPurchaseUnresolved) {
		return purchaseUnresolved(err)
	}
	var partial *exchange.PartialExecutionError
	if err != nil && !errors.Is(err, exchange.ErrExecutionRejected) && !errors.As(err, &partial) {
		return fmt.Errorf("an earlier request is unresolved; run recover first: %w", err)
	}
	submitCtx, cancel := context.WithTimeout(ctx, 30*time.Second)
	err = client.Exchange(submitCtx, action)
	cancel()
	if client.LastReceipt != nil {
		fmt.Fprintf(out, "Receipt: %s\n", client.LastReceipt.Raw)
	}
	switch {
	case err == nil:
		fmt.Fprintln(out, "Purchase accepted")
	case errors.Is(err, exchange.ErrPurchaseUnresolved):
		return purchaseUnresolved(err)
	default:
		return fmt.Errorf("purchase not confirmed: %w", err)
	}
	after, err := printQuota(ctx, client, out, "after")
	if err != nil {
		return err
	}
	fmt.Fprintf(out, "Remaining requests: %d before, %d after\n", before.Remaining(), after.Remaining())
	return nil
}

func printQuota(ctx context.Context, client *exchange.Client, out io.Writer, label string) (exchange.UserRateLimit, error) {
	limit, err := client.UserRateLimit(ctx)
	if err != nil {
		return limit, fmt.Errorf("read userRateLimit: %w", err)
	}
	fmt.Fprintf(out, "Quota %s for %s: cumVlm=%s cap=%d used=%d surplus=%d remaining=%d\n",
		label, client.AccountAddress(), limit.CumVlm, limit.Cap, limit.Used, limit.Surplus, limit.Remaining())
	return limit, nil
}

// purchaseUnresolved explains what to do with a purchase of unknown outcome.
func purchaseUnresolved(cause error) error {
	var unknown *exchange.UnknownOutcomeError
	nonce := uint64(0)
	if errors.As(cause, &unknown) {
		nonce = unknown.Nonce
	}
	return fmt.Errorf("a request purchase (nonce=%d) has an unknown outcome and stays pending; it is never resent. "+
		"Compare nRequestsSurplus and nRequestsUsed from `mmcli quota` with the values from before the attempt to decide whether it was applied, "+
		"then run `ACK_PURCHASE_NONCE=%d mmcli recover` to clear it: %w", nonce, nonce, cause)
}

// acknowledgePurchase clears a pending purchase after the operator compared
// userRateLimit. It sends nothing.
func acknowledgePurchase(client *exchange.Client, text string) error {
	nonce, err := strconv.ParseUint(text, 10, 64)
	if err != nil {
		return fmt.Errorf("ACK_PURCHASE_NONCE must be the nonce printed with the unresolved purchase")
	}
	if err := client.AcknowledgePendingPurchase(nonce, "acknowledged from mmcli recover after comparing userRateLimit"); err != nil {
		return err
	}
	fmt.Fprintf(os.Stdout, "Pending purchase nonce=%d cleared; nothing was sent\n", nonce)
	return nil
}
