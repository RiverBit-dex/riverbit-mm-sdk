package exchange

import (
	"bytes"
	"context"
	"encoding/json"
	"errors"
	"fmt"
	"math"
	"strconv"
	"strings"
)

// Request quota
//
// Each account (the master account, or the vault when trading for a vault; API
// wallets count against the account they act for) has a cumulative request cap
// of 10000 + 1 per USD traded. Units: an order action counts one per leg, a
// modify one, a batchModify one per entry, and cancel and cancelByCloid one per
// entry. Cancels have a higher cap: min(cap + 100000, 2 * cap). Once an account
// is over the cap, one unit is accepted every 10 seconds. Purchased requests
// (see ReserveRequestWeight) add to both caps.
//
// The SDK never retries, waits or buys requests on its own. When an action is
// rejected for quota (IsQuotaRejection), the caller decides: back off (one
// unit per 10 seconds is still accepted), trade to raise the cap, or buy
// requests.

// Execution-layer codes of the request quota.
const (
	// CodeRequestQuotaExhausted is code 1800: the account is over its
	// cumulative request cap.
	CodeRequestQuotaExhausted = 1800
	// CodeReserveWeightOverflow is code 1801: the purchase would overflow the
	// purchased total.
	CodeReserveWeightOverflow = 1801
)

// QuotaRejectionText is the text every quota rejection contains. The rejection
// can be wrapped, for example "action rejected by execution (code 1800) (...)"
// or "action rejected at ingress (code 1800) (...)", and a cancel leg appends
// " asset=N", so it is matched as a substring, never as a prefix.
const QuotaRejectionText = "Too many cumulative requests sent"

// ErrRequestQuotaExceeded is matched by errors.Is for every final rejection
// caused by the request quota: *ActionRejectedError, *PartialExecutionError
// with a quota-rejected leg, and the error wrapping ErrExecutionRejected. It is
// a final rejection of the rejected legs, never an unknown outcome: nothing
// executed for them and a new request gets a new nonce.
var ErrRequestQuotaExceeded = errors.New("request quota exceeded (code 1800): back off, trade to raise the cap, or buy requests")

// IsQuotaText reports whether a rejection text (a per-leg error or a whole
// action rejection) is a request quota rejection.
func IsQuotaText(text string) bool { return strings.Contains(text, QuotaRejectionText) }

// IsQuotaRejection reports whether err is, or contains, a request quota
// rejection. It is the same as errors.Is(err, ErrRequestQuotaExceeded).
func IsQuotaRejection(err error) bool { return errors.Is(err, ErrRequestQuotaExceeded) }

// IsQuotaRejection reports whether the leg is rejected for request quota.
func (l LegStatus) IsQuotaRejection() bool { return l.Kind == "error" && IsQuotaText(l.Error) }

// IsQuotaRejection reports whether the leg was rejected for request quota.
func (l LegOutcome) IsQuotaRejection() bool { return IsQuotaText(l.Error) }

// IsQuotaRejection reports whether the reply carries a quota rejection: a leg
// error, or a whole-action rejection such as the one of a modify.
func (r *Receipt) IsQuotaRejection() bool {
	return r != nil && bytes.Contains(r.Raw, []byte(QuotaRejectionText))
}

// IsQuotaRejection reports whether the whole action was rejected for request
// quota. The original text stays in Response.
func (e *ActionRejectedError) IsQuotaRejection() bool {
	return bytes.Contains(e.Response, []byte(QuotaRejectionText))
}

// Is makes errors.Is(err, ErrRequestQuotaExceeded) work.
func (e *ActionRejectedError) Is(target error) bool {
	return target == ErrRequestQuotaExceeded && e.IsQuotaRejection()
}

// IsQuotaRejection reports whether at least one rejected leg was rejected for
// request quota. Legs after the first rejected one are never executed.
func (e *PartialExecutionError) IsQuotaRejection() bool {
	for _, l := range e.Rejected {
		if l.IsQuotaRejection() {
			return true
		}
	}
	return false
}

// Is makes errors.Is(err, ErrRequestQuotaExceeded) work.
func (e *PartialExecutionError) Is(target error) bool {
	return target == ErrRequestQuotaExceeded && e.IsQuotaRejection()
}

// UserRateLimit is the answer of the userRateLimit query.
//
// CumVlm is the cumulative traded volume in USD as a decimal string. Cap is the
// cap for ordinary actions, 10000 + floor(CumVlm), purchased requests not
// included. Used is max(0, used - purchased) and Surplus is max(0, purchased -
// used), so at most one of them is above zero.
type UserRateLimit struct {
	CumVlm  string
	Cap     uint64 // nRequestsCap
	Used    uint64 // nRequestsUsed
	Surplus uint64 // nRequestsSurplus
}

// Remaining is the number of ordinary requests left:
// max(0, Cap - Used) + Surplus, with saturating arithmetic.
func (r UserRateLimit) Remaining() uint64 {
	var left uint64
	if r.Cap > r.Used {
		left = r.Cap - r.Used
	}
	if left > math.MaxUint64-r.Surplus {
		return math.MaxUint64
	}
	return left + r.Surplus
}

// UnmarshalJSON accepts the counts as JSON numbers or decimal strings.
func (r *UserRateLimit) UnmarshalJSON(data []byte) error {
	var raw struct {
		CumVlm  json.RawMessage `json:"cumVlm"`
		Cap     json.RawMessage `json:"nRequestsCap"`
		Used    json.RawMessage `json:"nRequestsUsed"`
		Surplus json.RawMessage `json:"nRequestsSurplus"`
	}
	if err := json.Unmarshal(data, &raw); err != nil {
		return fmt.Errorf("userRateLimit: %w", err)
	}
	var out UserRateLimit
	out.CumVlm = strings.Trim(string(raw.CumVlm), `"`)
	if out.CumVlm == "" || out.CumVlm == "null" {
		return fmt.Errorf("userRateLimit: missing cumVlm")
	}
	for _, f := range []struct {
		name string
		raw  json.RawMessage
		dst  *uint64
	}{{"nRequestsCap", raw.Cap, &out.Cap}, {"nRequestsUsed", raw.Used, &out.Used}, {"nRequestsSurplus", raw.Surplus, &out.Surplus}} {
		n, err := parseCount(f.raw)
		if err != nil {
			return fmt.Errorf("userRateLimit %s: %w", f.name, err)
		}
		*f.dst = n
	}
	*r = out
	return nil
}

func parseCount(raw json.RawMessage) (uint64, error) {
	text := strings.TrimSpace(string(raw))
	text = strings.Trim(text, `"`)
	if text == "" || text == "null" {
		return 0, fmt.Errorf("missing")
	}
	n, err := strconv.ParseUint(text, 10, 64)
	if err != nil {
		return 0, fmt.Errorf("not an unsigned 64-bit integer: %q", text)
	}
	return n, nil
}

// Request purchase

const (
	// MaxReserveWeight is the largest number of requests one purchase may buy.
	MaxReserveWeight = 1_000_000_000
	// RequestPriceMicroUSDC is the price of one purchased request in micro-USDC
	// (0.0005 USDC).
	RequestPriceMicroUSDC = 500
)

// ReserveCostMicroUSDC returns the price of weight requests in micro-USDC
// (weight * 500). It saturates instead of overflowing; a valid weight never does.
func ReserveCostMicroUSDC(weight uint64) uint64 {
	if weight > math.MaxUint64/RequestPriceMicroUSDC {
		return math.MaxUint64
	}
	return weight * RequestPriceMicroUSDC
}

// ReserveRequestWeight is the action that buys request quota. Weight is the
// number of requests, an integer JSON number (1 to MaxReserveWeight).
// Destination, if set, is the account that receives the requests; empty means
// the payer. Field order is the signing key order.
//
// Price: RequestPriceMicroUSDC per request (0.0005 USDC), paid by the payer. The
// payer is the signer's account (for an API wallet, its master account). A
// client created with NewForVault pays from the vault and the requests go to the
// vault; its Destination must be empty or the vault. To have the master pay
// for a vault, use a client without a vault and set Destination to the vault.
// Paying from a vault is a per-network setting and may be rejected.
//
// The action has no legs and does not consume quota. Reply: status "ok" with
// {"type":"default"} is done; "err", or an error entry (for example code 1801),
// is a final rejection. Any other outcome, a timeout or a malformed reply is an
// unknown outcome: it is never resent, and nothing can be looked up by ID.
// Compare UserRateLimit (Surplus and Used) before and after to find out whether
// the purchase was applied.
type ReserveRequestWeight struct {
	Type        string `json:"type" msgpack:"type"`
	Weight      uint64 `json:"weight" msgpack:"weight"`
	Destination string `json:"destination,omitempty" msgpack:"destination,omitempty"`
}

// NewReserveRequestWeight returns the action buying weight requests for the
// payer. Validation happens when the action is signed (see Validate).
func NewReserveRequestWeight(weight uint64) *ReserveRequestWeight {
	return &ReserveRequestWeight{Type: "reserveRequestWeight", Weight: weight}
}

// NewReserveRequestWeightFor returns the action buying weight requests that
// the account destination receives; the destination is lowercased.
func NewReserveRequestWeightFor(weight uint64, destination string) *ReserveRequestWeight {
	a := NewReserveRequestWeight(weight)
	a.Destination = strings.ToLower(destination)
	return a
}

// CostMicroUSDC is the price of the purchase in micro-USDC.
func (a *ReserveRequestWeight) CostMicroUSDC() uint64 { return ReserveCostMicroUSDC(a.Weight) }

// Validate checks the action before it is signed: 1 <= Weight <= MaxReserveWeight
// and Destination, if set, a lowercase 0x-prefixed 20-byte address (the signed
// bytes and the sent bytes are the same bytes, so it is never lowercased
// afterwards).
func (a *ReserveRequestWeight) Validate() error {
	if a.Type != "reserveRequestWeight" {
		return fmt.Errorf("reserveRequestWeight: wrong type %q", a.Type)
	}
	if a.Weight < 1 || a.Weight > MaxReserveWeight {
		return fmt.Errorf("reserveRequestWeight: weight %d out of range 1..%d", a.Weight, MaxReserveWeight)
	}
	if a.Destination != "" {
		if a.Destination != strings.ToLower(a.Destination) {
			return fmt.Errorf("reserveRequestWeight: destination must be lowercase")
		}
		if _, err := normalizeAddress(a.Destination); err != nil {
			return fmt.Errorf("reserveRequestWeight: invalid destination")
		}
	}
	return nil
}

func reserveAction(action any) (*ReserveRequestWeight, bool) {
	switch a := action.(type) {
	case ReserveRequestWeight:
		return &a, true
	case *ReserveRequestWeight:
		return a, a != nil
	}
	return nil, false
}

// checkReserve validates a purchase for this client: the action itself and,
// for a client with a vault target, the destination rule.
func (c *Client) checkReserve(a *ReserveRequestWeight) error {
	if err := a.Validate(); err != nil {
		return err
	}
	if c.account != "" && a.Destination != "" && a.Destination != c.account {
		return fmt.Errorf("reserveRequestWeight: a client with a vault target pays from the vault: destination must be empty or the vault (use a client without a vault to pay from the master)")
	}
	return nil
}

// UserRateLimit queries the request quota of user: the master account or a
// vault, not an API wallet. A node that cannot answer replies with HTTP 501,
// which is returned as an ordinary read error.
func (c *InfoClient) UserRateLimit(ctx context.Context, user string) (UserRateLimit, error) {
	var out UserRateLimit
	err := c.Query(ctx, map[string]any{"type": "userRateLimit", "user": user}, &out)
	return out, err
}

// UserRateLimit queries the request quota of AccountAddress over REST.
func (c *Client) UserRateLimit(ctx context.Context) (UserRateLimit, error) {
	return c.InfoClient().UserRateLimit(ctx, c.AccountAddress())
}

// judgeReserve turns the reply to a reserveRequestWeight into one outcome.
// status "ok" with {"type":"default"} and no error entry is done (nil); "err",
// or an error entry, is a final rejection, unless the text asks the caller to
// query first or reports a duplicate nonce; everything else is unknown.
func (c *Client) judgeReserve(action any, n uint64, status string, response, committed json.RawMessage) error {
	if status != "ok" && status != "err" {
		return c.unknown(action, n, fmt.Errorf("exchange: %s %s", status, response))
	}
	var body struct {
		Type string `json:"type"`
		Data *struct {
			Statuses []json.RawMessage `json:"statuses"`
		} `json:"data"`
	}
	if json.Unmarshal(response, &body) == nil && body.Data != nil {
		// A purchase has no legs; an error entry means it was rejected.
		for _, raw := range body.Data.Statuses {
			if leg := parseLeg(raw); leg.Kind == "error" && leg.Error != "" {
				quoted, _ := json.Marshal(leg.Error)
				if code, final := finalRejection("err", quoted, nil); final {
					return &ActionRejectedError{Status: status, Response: append(json.RawMessage(nil), response...), Code: code}
				}
			}
		}
		return c.unknown(action, n, fmt.Errorf("exchange reply to a purchase carries statuses: %s", response))
	}
	if status == "err" {
		if code, final := finalRejection(status, response, committed); final {
			return &ActionRejectedError{Status: status, Response: response, Code: code}
		}
		return c.unknown(action, n, fmt.Errorf("exchange: %s %s", status, response))
	}
	if body.Type == "default" && isDefaultAck(response) {
		return nil
	}
	return c.unknown(action, n, fmt.Errorf("exchange response: %s", response))
}
