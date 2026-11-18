package exchange

import (
	"context"
	"errors"
	"fmt"
	"net/http"
	"net/http/httptest"
	"strings"
	"sync/atomic"
	"testing"
	"time"
)

func TestParseReceiptKeepsEveryLeg(t *testing.T) {
	raw := `{"status":"ok","response":{"type":"order","data":{"statuses":[` +
		`{"resting":{"oid":101}},{"filled":{"oid":102,"totalSz":"0.5","avgPx":"80000.5"}},` +
		`{"error":"Order must have minimum value of $10."},"waitingForTrigger"]}}}`
	receipt, err := ParseReceipt([]byte(raw))
	if err != nil {
		t.Fatal(err)
	}
	if receipt.Status != "ok" || receipt.Type != "order" || len(receipt.Legs) != 4 {
		t.Fatalf("receipt = %+v", receipt)
	}
	if l := receipt.Legs[0]; l.Kind != "resting" || l.OID != 101 {
		t.Fatalf("leg 0 = %+v", l)
	}
	if l := receipt.Legs[1]; l.Kind != "filled" || l.OID != 102 || l.TotalSz != "0.5" || l.AvgPx != "80000.5" {
		t.Fatalf("leg 1 = %+v", l)
	}
	if l := receipt.Legs[2]; l.Kind != "error" || !strings.Contains(l.Error, "minimum value") {
		t.Fatalf("leg 2 = %+v", l)
	}
	if receipt.Legs[3].Kind != "waitingForTrigger" || len(receipt.Rejected()) != 1 {
		t.Fatalf("legs = %+v", receipt.Legs)
	}
	// A whole-action rejection has no legs; a committed result is read from err replies.
	whole, err := ParseReceipt([]byte(`{"status":"err","response":"bad"}`))
	if err != nil || whole.Status != "err" || len(whole.Legs) != 0 {
		t.Fatalf("whole-action rejection = %+v, %v", whole, err)
	}
	committed, err := ParseReceipt([]byte(`{"status":"err","response":"query the order status","committedResult":{"type":"order","data":{"statuses":[{"resting":{"oid":7}}]}}}`))
	if err != nil || len(committed.Legs) != 1 || committed.Legs[0].OID != 7 {
		t.Fatalf("committed result = %+v, %v", committed, err)
	}
}

func TestClientRefusesToSignWithoutChainID(t *testing.T) {
	client, err := newTestClient("https://example.invalid")
	if err != nil {
		t.Fatal(err)
	}
	if _, err := client.Sign(NewPlace(LimitOrder(0, true, "1", "1", "Alo")), 1); !errors.Is(err, ErrNoChainID) {
		t.Fatalf("err = %v", err)
	}
}

// A whole-action rejection is final: *ActionRejectedError without a journal,
// ErrExecutionRejected with one, and code 91 is called out.
func TestWholeActionRejectionIsFinal(t *testing.T) {
	for _, durable := range []bool{false, true} {
		t.Run(fmt.Sprintf("durable=%t", durable), func(t *testing.T) {
			server := httptest.NewServer(http.HandlerFunc(func(w http.ResponseWriter, r *http.Request) {
				fmt.Fprint(w, `{"status":"err","response":"action rejected by execution (code 91): User or API Wallet does not exist"}`)
			}))
			defer server.Close()
			client, err := newTestClient(server.URL)
			if err != nil {
				t.Fatal(err)
			}
			client.SignChainID = 12589
			if durable {
				client.StateDir = t.TempDir()
			}
			err = client.Exchange(context.Background(), NewPlace(LimitOrder(0, true, "100", "1", "Alo")))
			var rejected *ActionRejectedError
			if durable {
				if !errors.Is(err, ErrExecutionRejected) || !errors.Is(err, ErrSignerNotRecognized) {
					t.Fatalf("err = %v", err)
				}
				return
			}
			if !errors.As(err, &rejected) || rejected.Code != CodeUnknownSigner {
				t.Fatalf("err = %v", err)
			}
		})
	}
}

// Without a journal an unreadable reply still must not look like a rejection:
// the error carries the nonce, the exact signed action and the cloids.
func TestUnknownOutcomeWithoutJournalCarriesWhatReconciliationNeeds(t *testing.T) {
	server := httptest.NewServer(http.HandlerFunc(func(w http.ResponseWriter, r *http.Request) {
		http.Error(w, "bad gateway", http.StatusBadGateway)
	}))
	defer server.Close()
	client, err := newTestClient(server.URL)
	if err != nil {
		t.Fatal(err)
	}
	client.SignChainID = 12589
	order := LimitOrder(0, true, "100", "1", "Alo")
	order.Cloid = "0x00000000000000000000000000000001"
	action := NewPlace(order)
	err = client.Exchange(context.Background(), action)
	var unknown *UnknownOutcomeError
	var rejected *ActionRejectedError
	if !errors.As(err, &unknown) || errors.As(err, &rejected) {
		t.Fatalf("err = %v", err)
	}
	if unknown.Nonce == 0 || len(unknown.Cloids) != 1 || unknown.Cloids[0] != order.Cloid || !strings.HasPrefix(string(unknown.Action), `{"type":"order"`) {
		t.Fatalf("unknown outcome = %+v", unknown)
	}
}

// A 429 on a signed action is not retried and not proof of non-execution: it is
// an unknown outcome, even when the reply carries Retry-After.
func TestRateLimitedWriteIsAnUnknownOutcome(t *testing.T) {
	var calls atomic.Int32
	server := httptest.NewServer(http.HandlerFunc(func(w http.ResponseWriter, r *http.Request) {
		calls.Add(1)
		w.Header().Set("Retry-After", "1")
		http.Error(w, "slow down", http.StatusTooManyRequests)
	}))
	defer server.Close()
	client, err := newTestClient(server.URL)
	if err != nil {
		t.Fatal(err)
	}
	client.SignChainID = 12589
	order := LimitOrder(0, true, "100", "1", "Alo")
	order.Cloid = "0x00000000000000000000000000000001"
	err = client.Exchange(context.Background(), NewPlace(order))
	var unknown *UnknownOutcomeError
	var rejected *ActionRejectedError
	if !errors.As(err, &unknown) || errors.As(err, &rejected) || calls.Load() != 1 {
		t.Fatalf("err = %v, calls = %d", err, calls.Load())
	}
	if unknown.Nonce == 0 || len(unknown.Cloids) != 1 {
		t.Fatalf("unknown outcome = %+v", unknown)
	}
}

func TestRateLimitedReadsHonorRetryAfter(t *testing.T) {
	var calls atomic.Int32
	server := httptest.NewServer(http.HandlerFunc(func(w http.ResponseWriter, r *http.Request) {
		if calls.Add(1) == 1 {
			w.Header().Set("Retry-After", "1")
			http.Error(w, "slow down", http.StatusTooManyRequests)
			return
		}
		fmt.Fprint(w, `{"nextAgentChainId":12589,"agentChainId":12580}`)
	}))
	defer server.Close()
	start := time.Now()
	id, err := NewInfoClient(server.URL).ChainIdentity(context.Background())
	if err != nil || id.SignChainID() != 12589 {
		t.Fatalf("identity = %+v, %v", id, err)
	}
	if calls.Load() != 2 || time.Since(start) < time.Second {
		t.Fatalf("calls=%d after %v; the retry must wait for Retry-After", calls.Load(), time.Since(start))
	}
}

func TestRateLimitedReadsBackOffWithoutRetryAfter(t *testing.T) {
	var calls atomic.Int32
	server := httptest.NewServer(http.HandlerFunc(func(w http.ResponseWriter, r *http.Request) {
		if calls.Add(1) <= 2 {
			http.Error(w, "slow down", http.StatusTooManyRequests) // no Retry-After header
			return
		}
		fmt.Fprint(w, `{"nextAgentChainId":12589}`)
	}))
	defer server.Close()
	start := time.Now()
	id, err := NewInfoClient(server.URL).ChainIdentity(context.Background())
	if err != nil || id.SignChainID() != 12589 {
		t.Fatalf("identity = %+v, %v", id, err)
	}
	// Backoff starts at 250 ms and doubles: 250 ms + 500 ms before the third call.
	if elapsed := time.Since(start); calls.Load() != 3 || elapsed < 700*time.Millisecond || elapsed > 5*time.Second {
		t.Fatalf("calls=%d after %v; expected two backoff waits (about 750 ms)", calls.Load(), elapsed)
	}
}

func TestRetryRateLimitedPrefersRetryAfterOverBackoff(t *testing.T) {
	calls := 0
	start := time.Now()
	err := retryRateLimited(context.Background(), func() error {
		calls++
		if calls == 1 {
			return &RateLimitError{Path: "/info", RetryAfter: 30 * time.Millisecond}
		}
		return nil
	})
	// The first backoff step would be 250 ms; a shorter Retry-After is honored as sent.
	if elapsed := time.Since(start); err != nil || calls != 2 || elapsed >= 200*time.Millisecond {
		t.Fatalf("err=%v calls=%d elapsed=%v", err, calls, elapsed)
	}
}

func TestParseRetryAfter(t *testing.T) {
	now := time.Date(2026, 1, 2, 3, 4, 5, 0, time.UTC)
	for value, want := range map[string]time.Duration{
		"":                              0,
		"3":                             3 * time.Second,
		" 7 ":                           7 * time.Second,
		"0":                             0,
		"-1":                            0,
		"soon":                          0,
		"Fri, 02 Jan 2026 03:04:15 GMT": 10 * time.Second,
		"Fri, 02 Jan 2026 03:04:00 GMT": 0,
	} {
		if got := parseRetryAfter(value, now); got != want {
			t.Errorf("parseRetryAfter(%q) = %v, want %v", value, got, want)
		}
	}
}

func TestRateLimitErrorSurfacesWhenTheContextEnds(t *testing.T) {
	server := httptest.NewServer(http.HandlerFunc(func(w http.ResponseWriter, r *http.Request) {
		http.Error(w, "slow down", http.StatusTooManyRequests)
	}))
	defer server.Close()
	ctx, cancel := context.WithTimeout(context.Background(), 100*time.Millisecond)
	defer cancel()
	_, err := NewInfoClient(server.URL).ChainIdentity(ctx)
	var limited *RateLimitError
	if !errors.Is(err, context.DeadlineExceeded) || !errors.As(err, &limited) || limited.Path != "/info" {
		t.Fatalf("err = %v", err)
	}
}

func TestAssetLookup(t *testing.T) {
	server := httptest.NewServer(http.HandlerFunc(func(w http.ResponseWriter, r *http.Request) {
		fmt.Fprint(w, `[{"universe":[{"name":"RBT","szDecimals":2,"maxLeverage":20},{"name":"BTC","szDecimals":5,"maxLeverage":40}]},[]]`)
	}))
	defer server.Close()
	info := NewInfoClient(server.URL + "/")
	asset, err := info.Asset(context.Background(), "BTC")
	if err != nil || asset != (Asset{Index: 1, Name: "BTC", SzDecimals: 5, MaxLeverage: 40}) {
		t.Fatalf("asset = %+v, %v", asset, err)
	}
	if _, err := info.Asset(context.Background(), "NOPE"); err == nil {
		t.Fatal("unknown coin must fail")
	}
	if err := info.Check(context.Background()); err != nil {
		t.Fatal(err)
	}
}

func TestNextNonceIsStrictlyIncreasingAndPersistedWithAStateDir(t *testing.T) {
	client, err := newTestClient("https://example.invalid")
	if err != nil {
		t.Fatal(err)
	}
	client.StateDir = t.TempDir()
	first, err := client.NextNonce()
	if err != nil {
		t.Fatal(err)
	}
	second, err := client.NextNonce()
	if err != nil || second <= first {
		t.Fatalf("nonces %d, %d (%v)", first, second, err)
	}
	// A fresh client of the same signer starts above the persisted floor.
	fresh, err := newTestClient("https://example.invalid")
	if err != nil {
		t.Fatal(err)
	}
	fresh.StateDir = client.StateDir
	third, err := fresh.NextNonce()
	if err != nil || third <= second {
		t.Fatalf("nonce %d after %d (%v)", third, second, err)
	}
}
