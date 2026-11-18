package exchange

import (
	"bytes"
	"encoding/json"
	"errors"
	"fmt"
	"regexp"
	"strings"
	"time"
)

// ErrExecutionRejected marks a request in which no leg executed: the execution
// layer refused it as a whole, or every leg was rejected. Nothing ran, the
// pending journal entry was retired, and the next request gets a new nonce. It
// is wrapped by the error Exchange returns when a StateDir is configured;
// unwrap with errors.Is. Without a StateDir the same condition is reported as
// *ActionRejectedError. When only some legs were rejected, the error is a
// *PartialExecutionError instead, never ErrExecutionRejected.
var ErrExecutionRejected = errors.New("execution rejected")

// CodeDuplicateNonce is the execution-layer code 205: a request with this nonce
// was already seen. The original request may have executed, so the code is
// never a final rejection of the action; the outcome is unknown and must be
// reconciled.
const CodeDuplicateNonce = 205

// ErrPendingUnresolved is returned when an earlier request of the same signer
// and account is still unresolved. Call RecoverPending before any new action.
var ErrPendingUnresolved = errors.New("unresolved submission: recover before another action")

// CodeUnknownSigner is the execution-layer code 91: the signer or user is not
// known to the chain, or the signing image does not match (wrong key order,
// wrong chain ID, wrong vault tail). It is deterministic and final.
const CodeUnknownSigner = 91

// ErrSignerNotRecognized accompanies ErrExecutionRejected when the rejection
// carries code 91. Check the API wallet authorization, the master account,
// the key order of the signed action and the chain ID from chainIdentity.
// Code 91 does not by itself mean missing funds.
var ErrSignerNotRecognized = errors.New("code 91: user or API wallet not recognized, or the signing image does not match (check API wallet authorization, master account, key order and chain ID)")

// ActionRejectedError is a gateway reply status="err" that carries the
// execution layer's explicit whole-action rejection. Nothing executed, so the
// request is final. Per-leg rejections are not whole-action rejections: they
// appear in the receipt.
type ActionRejectedError struct {
	Status   string          // reply status, "err"
	Response json.RawMessage // reply body
	Code     int             // execution-layer code, 0 if the reply carries none
}

// Error implements error.
func (e *ActionRejectedError) Error() string {
	return fmt.Sprintf("exchange: %s %s", e.Status, e.Response)
}

// LegOutcome is the result of one leg of a request, as observed or as reported
// in the receipt. Legs are indexed in request order.
type LegOutcome struct {
	Index  int    // position of the leg in the request
	Cloid  string // client order ID of the leg, if it has one
	OID    uint64 // order ID, zero if none is known
	Status string // "open", "filled", "canceled", "rejected", "resting", "waitingForFill", ...
	Error  string // rejection text, empty unless the leg was rejected
}

// PartialExecutionError reports a request that was accepted but is not a clean
// success on every leg. Partial success is not rolled back, so it is neither a
// success nor a rejection of the whole request: nothing may be resent blindly.
// The pending journal entry, if any, was resolved and cleared.
//
// Executed lists legs that ran, Rejected the legs that did not, and Tracking
// the legs that were accepted but wait (waitingForFill, waitingForTrigger) and
// must be tracked through order events or orderStatus. The error is returned
// only when at least one leg executed or needs tracking; a request in which
// every leg was rejected is ErrExecutionRejected or *ActionRejectedError.
type PartialExecutionError struct {
	Nonce    uint64
	Executed []LegOutcome
	Rejected []LegOutcome
	Tracking []LegOutcome
}

// Error implements error.
func (e *PartialExecutionError) Error() string {
	describe := func(legs []LegOutcome) string {
		parts := make([]string, len(legs))
		for i, l := range legs {
			parts[i] = fmt.Sprintf("#%d", l.Index)
			if l.Cloid != "" {
				parts[i] += " cloid=" + l.Cloid
			}
			if l.OID != 0 {
				parts[i] += fmt.Sprintf(" oid=%d", l.OID)
			}
			if l.Error != "" {
				parts[i] += " (" + l.Error + ")"
			}
		}
		return strings.Join(parts, "; ")
	}
	return fmt.Sprintf("partial execution (nonce=%d): %d executed [%s], %d rejected [%s], %d need tracking [%s]",
		e.Nonce, len(e.Executed), describe(e.Executed), len(e.Rejected), describe(e.Rejected), len(e.Tracking), describe(e.Tracking))
}

// RateLimitError reports an HTTP 429 or a bin-ws "429" error reply. Reads
// retry it automatically (honoring RetryAfter); it is returned when the
// caller's context ends first, or from a write. A rate-limited write is not
// proof of non-execution unless the server answered before processing it; the
// pending journal treats it as an unknown outcome and reconciles.
type RateLimitError struct {
	Path       string        // "/info", "/exchange" or "bin-ws"
	RetryAfter time.Duration // from the Retry-After header; zero if absent
	Body       string        // start of the response body
}

// Error implements error.
func (e *RateLimitError) Error() string {
	if e.Path == "bin-ws" {
		return fmt.Sprintf("bin-ws rate limited: %s", e.Body)
	}
	return fmt.Sprintf("%s HTTP 429: %s", e.Path, e.Body)
}

// UnknownOutcomeError means a signed request may or may not have executed:
// the transport failed or timed out, the request was redirected, the reply or
// receipt was malformed or short, or the node answered with a status that asks
// the client to query before retrying, such as a duplicate nonce (code 205). The
// request must never be re-signed with a new nonce. Reconcile it by order ID
// or client order ID; with a StateDir configured it stays in the pending
// journal and RecoverPending resolves it.
type UnknownOutcomeError struct {
	Nonce   uint64          // nonce of the signed request
	Account string          // account the request acted for (master or vault)
	Action  json.RawMessage // the exact signed action JSON
	Cloids  []string        // client order IDs to look up
	Cause   error           // what made the outcome unknown
}

// Error implements error.
func (e *UnknownOutcomeError) Error() string {
	return fmt.Sprintf("outcome unknown (nonce=%d account=%s): %v", e.Nonce, e.Account, e.Cause)
}

// Unwrap returns the underlying cause, such as a context deadline.
func (e *UnknownOutcomeError) Unwrap() error { return e.Cause }

var executionRejectionRE = regexp.MustCompile(`action rejected by execution \(code (\d+)\)`)

// ambiguousErrHints are node diagnostics for status=err replies that may still
// carry effects: the action produced results, or a phase is only waiting. They
// tell the caller to query the order status, so the request is not final.
var ambiguousErrHints = []string{"query the order status", "waiting for its trigger", "not verified", "already pending", "duplicate nonce", "nonce already"}

// finalRejection reports whether a status=err reply means nothing executed. HTTP
// failures never get here (they are returned as errors). A reply with a
// committedResult with possible effects, or text asking to query before retrying, may
// have effects and stays ambiguous; every other plain err (for example "Attempted
// to modify to invalid new order") is the node refusing the action.
func finalRejection(status string, response, committed json.RawMessage) (int, bool) {
	if status != "err" {
		return 0, false
	}
	// A quota check runs before anything executes, so a quota rejection is
	// final even when the reply carries a committedResult member.
	if bytes.Contains(response, []byte(QuotaRejectionText)) {
		return CodeRequestQuotaExhausted, true
	}
	if len(committed) > 0 && string(committed) != "null" {
		return 0, committedOrderRejected(committed)
	}
	if code, ok := executionRejectionCode(status, response); ok {
		// A duplicate nonce says the original request was already seen; it
		// may have executed.
		return code, code != CodeDuplicateNonce
	}
	var text string
	if json.Unmarshal(response, &text) != nil || text == "" {
		return 0, false
	}
	for _, hint := range ambiguousErrHints {
		if strings.Contains(strings.ToLower(text), hint) {
			return 0, false
		}
	}
	return 0, true
}

// A rejected modify leg is wrapped in committedResult too. A nonempty order
// result containing only plain errors proves terminal rejection. Mixed
// outcomes, localCommitted hints, waiting states and unknown fields do not.
func committedOrderRejected(raw json.RawMessage) bool {
	var envelope map[string]json.RawMessage
	if json.Unmarshal(raw, &envelope) != nil || len(envelope) != 2 || string(envelope["type"]) != `"order"` {
		return false
	}
	var data map[string]json.RawMessage
	if json.Unmarshal(envelope["data"], &data) != nil || len(data) != 1 {
		return false
	}
	var statuses []map[string]json.RawMessage
	if json.Unmarshal(data["statuses"], &statuses) != nil || len(statuses) == 0 {
		return false
	}
	for _, leg := range statuses {
		if len(leg) != 1 {
			return false
		}
		if _, ok := finalRejection("err", leg["error"], nil); !ok {
			return false
		}
	}
	return true
}

// Only the exact deterministic phrase carries an execution code.
func executionRejectionCode(status string, response json.RawMessage) (int, bool) {
	if status != "err" {
		return 0, false
	}
	m := executionRejectionRE.FindSubmatch(response)
	if m == nil {
		return 0, false
	}
	var code int
	if _, err := fmt.Sscanf(string(m[1]), "%d", &code); err != nil {
		return 0, false
	}
	return code, true
}
