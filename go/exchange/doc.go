// Package exchange signs and submits RiverBit trading actions and reconciles
// the ones whose outcome is unknown.
//
// It provides:
//
//   - Signer: holds the API wallet or master key; signs bin-ws logins.
//   - Action types: Place (the order action) with its Order legs, Cancel,
//     CancelByCloid, Modify, BatchModify and ReserveRequestWeight (buys request
//     quota), plus Cloid. Field order is the
//     signing key order. Prices and sizes are decimal strings.
//   - InfoClient: unsigned REST /info queries (chain identity, asset metadata,
//     user role, open orders, userRateLimit, ...) with Retry-After handling.
//   - Client: signs actions (EIP-712), sends them over REST POST /exchange,
//     parses receipts, and keeps a durable pending journal so
//     that a request with an unknown outcome is reconciled by order ID or client
//     order ID after a timeout, a disconnect or a restart, never re-signed.
//   - Errors: *ActionRejectedError (the whole action was refused, final),
//     *PartialExecutionError (some legs ran, some were rejected or wait),
//     *RateLimitError (HTTP 429) and *UnknownOutcomeError (it may or may not
//     have executed; reconcile it). A request quota rejection (code 1800) is a
//     final rejection: IsQuotaRejection(err) tells it from the others.
//
// # Trading
//
// An API wallet signs; the master account (or a vault it leads) is the trading
// account. The master must authorize the API wallet first.
//
//	signer, err := exchange.NewSigner(keyHex)
//	client, err := exchange.New("https://pre-api.riverbit.io", signer)
//	client.StateDir = "/var/lib/mm/state" // one directory per signer, shared by all its processes
//	if err := client.LoadChainID(ctx); err != nil { // nextAgentChainId, required for signing
//		return err
//	}
//	if err := client.RecoverPending(ctx); err != nil { // resolve what an earlier run left unknown
//		return err
//	}
//	order := exchange.LimitOrder(assetIndex, true, "80000", "0.001", "Alo")
//	order.Cloid, err = exchange.NewCloid() // a fresh cloid per order, never reused
//	err = client.Exchange(ctx, exchange.NewPlace(order))
//	var unknown *exchange.UnknownOutcomeError
//	var partial *exchange.PartialExecutionError
//	switch {
//	case err == nil:
//		// executed; client.LastOrderIDs holds the order IDs
//	case errors.Is(err, exchange.ErrExecutionRejected):
//		// no leg executed; fix the cause, a new request gets a new nonce
//	case errors.As(err, &partial):
//		// some legs ran: partial.Executed, partial.Rejected, partial.Tracking
//	case errors.As(err, &unknown):
//		// keep going only after RecoverPending resolves it; never resend
//	}
//
// Look up the asset index with InfoClient.Asset at every startup; do not
// hard-code it.
//
// # Unknown outcomes
//
// A timeout, a disconnect, a redirect, a malformed or short receipt, a 429, a
// duplicate-nonce reply (code 205) or a node reply that asks to "query the
// order status" does not prove that a request failed. The
// request, with its nonce, goes into the journal before it is sent
// (Client.StateDir), and Exchange reconciles it by polling orderStatus until
// positive evidence arrives or the context ends. In the second case it
// returns *UnknownOutcomeError and the journal entry stays; RecoverPending
// continues from it, after a restart too. A new Exchange is refused with
// ErrPendingUnresolved until then. Without a StateDir the same condition is
// returned as *UnknownOutcomeError, but nothing is persisted: reconcile with
// the Cloids and Nonce it carries.
//
// A cloid names whichever order currently carries it. Use a fresh one per
// order (NewCloid) and never reuse a cloid while an earlier request with it is
// unresolved: recovery accepts an order found by cloid only if it was created
// after the request was journaled and has the submitted side, price and size;
// otherwise the request stays pending.
//
// # Request quota
//
// An account may send 10000 + 1 per USD traded requests in total (cancels
// have a higher cap); purchased requests add to it. InfoClient.UserRateLimit
// reports it and UserRateLimit.Remaining counts what is left. A request over the
// cap is rejected with code 1800 ("Too many cumulative requests sent"): a
// final rejection, never an unknown outcome, matched by IsQuotaRejection (and
// errors.Is with ErrRequestQuotaExceeded) on the whole-action rejection and on
// every rejected leg. An action can be partly accepted: the first legs run and
// every later leg carries the quota error. The SDK never retries or buys on its
// own; back off (one unit per 10 seconds is still accepted), trade, or buy:
//
//	err = client.Exchange(ctx, exchange.NewReserveRequestWeight(100000)) // 50 USDC
//
// A purchase has no legs and no order to look up. If its outcome is unknown it
// stays in the journal, RecoverPending never resends it and reports
// ErrPurchaseUnresolved: compare UserRateLimit (surplus and used) before and
// after, then call AcknowledgePendingPurchase.
//
// # Gateway credential
//
// RiverBit can issue a market maker a gateway credential that lifts the per-IP
// REST limits. Client.SetGatewayCredential and InfoClient.SetGatewayCredential
// make every REST request carry the header X-RiverBit-MM-Token. It is optional,
// never sent over bin-ws or to another URL, and never logged, journaled or put
// in an error message.
//
// The package never logs private keys or complete signed requests.
package exchange
