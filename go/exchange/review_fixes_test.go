package exchange

import (
	"context"
	"encoding/json"
	"errors"
	"fmt"
	"net/http"
	"net/http/httptest"
	"os"
	"strings"
	"sync/atomic"
	"testing"
	"time"
)

const reusedCloid = "0x11111111111111111111111111111111"

// lostReplyClient returns a journaled client whose /exchange reply is lost and
// whose orderStatus answers come from status.
func lostReplyClient(t *testing.T, status func(o Order) string) (*Client, Order, *atomic.Int32) {
	t.Helper()
	c := restVaultClient(t)
	c.RequireExecutionHeight = true
	order := alo("100")
	order.Cloid = reusedCloid
	var sends atomic.Int32
	server := httptest.NewServer(http.HandlerFunc(func(w http.ResponseWriter, r *http.Request) {
		if r.URL.Path == "/exchange" {
			sends.Add(1)
			http.Error(w, "reply lost", http.StatusBadGateway)
			return
		}
		fmt.Fprint(w, status(order))
	}))
	t.Cleanup(server.Close)
	c.URL = server.URL
	return c, order, &sends
}

// Finding: a reused cloid must not settle a new request with an old order.
func TestCloidHitMustBeTiedToThisRequest(t *testing.T) {
	now := time.Now().UnixMilli()
	body := func(side, price, size, origSz, cloid string, timestamp int64) func(Order) string {
		return func(Order) string {
			extra := ""
			if origSz != "" {
				extra += fmt.Sprintf(`,"origSz":%q`, origSz)
			}
			ts := ""
			if timestamp != 0 {
				ts = fmt.Sprintf(`,"timestamp":%d`, timestamp)
			}
			return fmt.Sprintf(`{"height":10,"status":"order","order":{"status":"open","order":{"oid":77,"side":%q,"limitPx":%q,"sz":%q,"cloid":%q%s%s}}}`,
				side, price, size, cloid, extra, ts)
		}
	}
	notTied := map[string]func(Order) string{
		"older than the request":  body("B", "100", "0.10", "", reusedCloid, now-time.Hour.Milliseconds()),
		"no timestamp":            body("B", "100", "0.10", "", reusedCloid, 0),
		"other side":              body("A", "100", "0.10", "", reusedCloid, now+1000),
		"other price":             body("B", "101", "0.10", "", reusedCloid, now+1000),
		"larger remaining size":   body("B", "100", "0.20", "", reusedCloid, now+1000),
		"other original size":     body("B", "100", "0.05", "0.20", reusedCloid, now+1000),
		"other cloid on the node": body("B", "100", "0.10", "", "0x22222222222222222222222222222222", now+1000),
		"leg result":              func(Order) string { return `{"height":10,"status":"legResult","leg":{"resting":{"oid":77}}}` },
	}
	for name, status := range notTied {
		t.Run("pending/"+name, func(t *testing.T) {
			c, order, sends := lostReplyClient(t, status)
			ctx, cancel := context.WithTimeout(context.Background(), 150*time.Millisecond)
			defer cancel()
			err := c.Exchange(ctx, Place{Type: "order", Orders: []Order{order}, Grouping: "na"})
			var unknown *UnknownOutcomeError
			if !errors.Is(err, context.DeadlineExceeded) || !errors.As(err, &unknown) || len(c.LastOrderIDs) != 0 {
				t.Fatalf("an order that is not tied to the request settled it: ids=%v err=%v", c.LastOrderIDs, err)
			}
			p, err := c.loadPending()
			if err != nil || p == nil || len(p.Evidence) != 0 {
				t.Fatalf("pending must stay with no evidence: %+v %v", p, err)
			}
			if sends.Load() != 1 {
				t.Fatalf("sends = %d", sends.Load())
			}
		})
	}
	tied := map[string]func(Order) string{
		"remaining size only": body("B", "100.0", "0.10", "", reusedCloid, now+1000),
		"partly filled":       body("B", "100", "0.04", "", reusedCloid, now+1000),
		"original size":       body("B", "100", "0", "0.1", reusedCloid, now+1000),
		"clock skew":          body("B", "100", "0.10", "", reusedCloid, now-2000),
	}
	for name, status := range tied {
		t.Run("settles/"+name, func(t *testing.T) {
			c, order, _ := lostReplyClient(t, status)
			ctx, cancel := context.WithTimeout(context.Background(), 2*time.Second)
			defer cancel()
			if err := c.Exchange(ctx, Place{Type: "order", Orders: []Order{order}, Grouping: "na"}); err != nil {
				t.Fatal(err)
			}
			if len(c.LastOrderIDs) != 1 || c.LastOrderIDs[0] != 77 {
				t.Fatalf("ids = %v", c.LastOrderIDs)
			}
		})
	}
}

// A journal without prepared_ms falls back to the nonce, a millisecond clock.
func TestCloidBindingFallsBackToNonce(t *testing.T) {
	p := &pendingRequest{Nonce: 1_700_000_000_000, Lookups: []string{reusedCloid}}
	order := alo("100")
	order.Cloid = reusedCloid
	p.Action, _ = json.Marshal(Place{Type: "order", Orders: []Order{order}, Grouping: "na"})
	bindings, err := p.legBindings()
	if err != nil || len(bindings) != 1 || bindings[0].SinceMs != 1_700_000_000_000 || !bindings[0].Buy {
		t.Fatalf("bindings = %+v, %v", bindings, err)
	}
	p.Lookups = []string{"0x33333333333333333333333333333333"}
	if _, err := p.legBindings(); err == nil {
		t.Fatal("cloid mismatch between the action and the lookups was accepted")
	}
}

func TestNewCloidIsFreshAndWellFormed(t *testing.T) {
	a, err := NewCloid()
	if err != nil {
		t.Fatal(err)
	}
	b, _ := NewCloid()
	var parsed Cloid
	if a == b || json.Unmarshal([]byte(`"`+a+`"`), &parsed) != nil || parsed.String() != a {
		t.Fatalf("cloids %q, %q", a, b)
	}
}

// Finding: code 205 (duplicate nonce) says the original may have executed.
func TestDuplicateNonceIsNeverAFinalRejection(t *testing.T) {
	for _, text := range []string{
		"action rejected by execution (code 205): duplicate nonce",
		"action rejected by execution (code 205)",
		"Duplicate nonce for this signer",
		"action already pending; it was not submitted again",
	} {
		quoted, _ := json.Marshal(text)
		if code, final := finalRejection("err", quoted, nil); final {
			t.Errorf("%q treated as final (code %d)", text, code)
		}
	}
	if code, final := finalRejection("err", json.RawMessage(`"action rejected by execution (code 91): unknown"`), nil); !final || code != CodeUnknownSigner {
		t.Errorf("code 91 must stay final: %d %t", code, final)
	}
	// Also as a leg error inside a committed result or a receipt.
	if committedOrderRejected(json.RawMessage(`{"type":"order","data":{"statuses":[{"error":"action rejected by execution (code 205)"}]}}`)) {
		t.Error("a duplicate-nonce leg proved rejection")
	}
	if _, err := parseCancelReceipt(json.RawMessage(`{"type":"cancel","data":{"statuses":[{"error":"action rejected by execution (code 205)"}]}}`), 1); err == nil {
		t.Error("a duplicate-nonce cancel leg accepted as terminal")
	}
	for _, durable := range []bool{false, true} {
		t.Run(fmt.Sprintf("durable=%t", durable), func(t *testing.T) {
			server := httptest.NewServer(http.HandlerFunc(func(w http.ResponseWriter, r *http.Request) {
				if r.URL.Path == "/exchange" {
					fmt.Fprint(w, `{"status":"err","response":"action rejected by execution (code 205): duplicate nonce"}`)
					return
				}
				fmt.Fprint(w, `{"height":5,"status":"unknownOid"}`)
			}))
			defer server.Close()
			c := restVaultClient(t)
			c.URL = server.URL
			if !durable {
				c = plainClient(t, server.URL)
			}
			ctx, cancel := context.WithTimeout(context.Background(), 100*time.Millisecond)
			defer cancel()
			err := c.Exchange(ctx, Place{Type: "order", Orders: []Order{alo("100")}, Grouping: "na"})
			var unknown *UnknownOutcomeError
			var rejected *ActionRejectedError
			if !errors.As(err, &unknown) || errors.As(err, &rejected) || errors.Is(err, ErrExecutionRejected) {
				t.Fatalf("err = %v; want an unknown outcome", err)
			}
			if durable {
				if p, err := c.loadPending(); err != nil || p == nil {
					t.Fatalf("pending must be retained: %v", err)
				}
			}
		})
	}
}

// Finding: partial execution is not a whole rejection.
func TestPlaceReceiptWithARejectedLegIsPartialExecution(t *testing.T) {
	const partialReceipt = `{"status":"ok","response":{"type":"order","data":{"statuses":[{"resting":{"oid":5}},{"error":"Price must be divisible by tick size."}]}}}`
	const rejectedReceipt = `{"status":"ok","response":{"type":"order","data":{"statuses":[{"error":"Price must be divisible by tick size."},{"error":"Price must be divisible by tick size."}]}}}`
	for name, tc := range map[string]struct {
		reply   string
		partial bool
	}{"one leg rejected": {partialReceipt, true}, "every leg rejected": {rejectedReceipt, false}} {
		t.Run(name, func(t *testing.T) {
			c := restVaultClient(t)
			server := httptest.NewServer(http.HandlerFunc(func(w http.ResponseWriter, r *http.Request) {
				if r.URL.Path == "/exchange" {
					fmt.Fprint(w, tc.reply)
					return
				}
				fmt.Fprint(w, `{"height":6,"status":"order","order":{"status":"open","order":{"oid":5}}}`)
			}))
			defer server.Close()
			c.URL = server.URL
			ctx, cancel := context.WithTimeout(context.Background(), 2*time.Second)
			defer cancel()
			err := c.Exchange(ctx, Place{Type: "order", Orders: []Order{alo("100"), alo("99")}, Grouping: "na"})
			var partial *PartialExecutionError
			if !tc.partial {
				if !errors.Is(err, ErrExecutionRejected) || errors.As(err, &partial) {
					t.Fatalf("nothing executed: want ErrExecutionRejected, got %v", err)
				}
			} else {
				if errors.Is(err, ErrExecutionRejected) || !errors.As(err, &partial) {
					t.Fatalf("want *PartialExecutionError, got %v", err)
				}
				if len(partial.Executed) != 1 || partial.Executed[0].OID != 5 || partial.Executed[0].Cloid == "" ||
					len(partial.Rejected) != 1 || partial.Rejected[0].Index != 1 || !strings.Contains(partial.Rejected[0].Error, "tick size") {
					t.Fatalf("per-leg result = %+v", partial)
				}
			}
			if _, statErr := os.Stat(c.pendingPath()); !os.IsNotExist(statErr) {
				t.Fatalf("resolved journal remains: %v", statErr)
			}
		})
	}
}

// Finding: a signed POST must never follow a redirect.
func TestExchangeNeverFollowsRedirects(t *testing.T) {
	var hits atomic.Int32
	target := httptest.NewServer(http.HandlerFunc(func(w http.ResponseWriter, r *http.Request) {
		hits.Add(1)
		fmt.Fprint(w, `{"status":"ok","response":{"type":"default"}}`)
	}))
	defer target.Close()
	for _, code := range []int{http.StatusMovedPermanently, http.StatusTemporaryRedirect, http.StatusPermanentRedirect} {
		for name, httpClient := range map[string]*http.Client{"default client": nil, "caller client that follows": {Timeout: time.Second}} {
			t.Run(fmt.Sprintf("%d/%s", code, name), func(t *testing.T) {
				var first atomic.Int32
				redirector := httptest.NewServer(http.HandlerFunc(func(w http.ResponseWriter, r *http.Request) {
					first.Add(1)
					http.Redirect(w, r, target.URL+r.URL.Path, code)
				}))
				defer redirector.Close()
				c, err := newTestClient(redirector.URL)
				if err != nil {
					t.Fatal(err)
				}
				c.SignChainID = 12589
				if httpClient != nil {
					c.HTTP = httpClient
				}
				err = c.Exchange(context.Background(), NewCancel(CancelItem{Asset: 0, OID: 1}))
				var unknown *UnknownOutcomeError
				if !errors.As(err, &unknown) || !strings.Contains(err.Error(), fmt.Sprint(code)) {
					t.Fatalf("a redirect must be an unknown outcome: %v", err)
				}
				if first.Load() != 1 || hits.Load() != 0 {
					t.Fatalf("redirector hits=%d, redirect target hits=%d", first.Load(), hits.Load())
				}
				// /info is not followed either.
				var out json.RawMessage
				if err := c.Info(context.Background(), map[string]string{"type": "meta"}, &out); err == nil || hits.Load() != 0 {
					t.Fatalf("/info followed a redirect: err=%v hits=%d", err, hits.Load())
				}
				if httpClient != nil && httpClient.CheckRedirect != nil {
					t.Fatal("the caller's client was modified")
				}
			})
		}
	}
}

// Finding: the journal-less path must judge the whole receipt.
func TestJournalLessReceiptIsJudgedLegByLeg(t *testing.T) {
	two := []Order{alo("100"), alo("99")}
	type result int
	const (
		ok result = iota
		unknown
		partial
		rejected
	)
	cases := []struct {
		name     string
		action   any
		statuses string
		want     result
	}{
		{"all resting", &Place{Type: "order", Orders: two, Grouping: "na"}, `[{"resting":{"oid":1}},{"filled":{"oid":2,"totalSz":"0.1","avgPx":"99"}}]`, ok},
		{"short receipt", &Place{Type: "order", Orders: two, Grouping: "na"}, `[{"resting":{"oid":1}}]`, unknown},
		{"long receipt", &Place{Type: "order", Orders: two[:1], Grouping: "na"}, `[{"resting":{"oid":1}},{"resting":{"oid":2}}]`, unknown},
		{"resting without oid", &Place{Type: "order", Orders: two, Grouping: "na"}, `[{"resting":{"oid":1}},{"resting":{}}]`, unknown},
		{"filled with oid zero", &Place{Type: "order", Orders: two, Grouping: "na"}, `[{"filled":{"oid":0}},{"resting":{"oid":2}}]`, unknown},
		{"malformed leg", &Place{Type: "order", Orders: two, Grouping: "na"}, `[{"resting":{"oid":1}},42]`, unknown},
		{"unknown kind", &Place{Type: "order", Orders: two, Grouping: "na"}, `[{"resting":{"oid":1}},{"mystery":{}}]`, unknown},
		{"query first", &Place{Type: "order", Orders: two, Grouping: "na"}, `[{"resting":{"oid":1}},{"error":"query the order status before retrying"}]`, unknown},
		{"duplicate nonce leg", &Place{Type: "order", Orders: two, Grouping: "na"}, `[{"resting":{"oid":1}},{"error":"action rejected by execution (code 205)"}]`, unknown},
		{"success on a place", &Place{Type: "order", Orders: two, Grouping: "na"}, `["success","success"]`, unknown},
		{"waiting for fill", &Place{Type: "order", Orders: two, Grouping: "na"}, `[{"resting":{"oid":1}},"waitingForFill"]`, partial},
		{"waiting for trigger", &Place{Type: "order", Orders: two, Grouping: "na"}, `["waitingForTrigger",{"resting":{"oid":1}}]`, partial},
		{"one rejected", &Place{Type: "order", Orders: two, Grouping: "na"}, `[{"error":"Price must be divisible by tick size."},{"resting":{"oid":2}}]`, partial},
		{"every leg rejected", &Place{Type: "order", Orders: two, Grouping: "na"}, `[{"error":"bad"},{"error":"bad"}]`, rejected},
		{"cancel ok", &Cancel{Type: "cancel", Cancels: []CancelItem{{OID: 1}, {OID: 2}}}, `["success","success"]`, ok},
		{"cancel short", &Cancel{Type: "cancel", Cancels: []CancelItem{{OID: 1}, {OID: 2}}}, `["success"]`, unknown},
		{"cancel with resting", &Cancel{Type: "cancel", Cancels: []CancelItem{{OID: 1}}}, `[{"resting":{"oid":1}}]`, unknown},
		{"cancel one rejected", &Cancel{Type: "cancel", Cancels: []CancelItem{{OID: 1}, {OID: 2}}}, `["success",{"error":"Order was never placed, already canceled, or filled."}]`, partial},
		{"modify one status", &Modify{Type: "modify", OID: 1, Order: alo("100")}, `[{"resting":{"oid":9}}]`, ok},
		{"batch modify short", &BatchModify{Type: "batchModify", Modifies: []ModifyItem{{OID: 1, Order: alo("100")}, {OID: 2, Order: alo("99")}}}, `[{"resting":{"oid":9}}]`, unknown},
	}
	for _, tc := range cases {
		t.Run(tc.name, func(t *testing.T) {
			server := httptest.NewServer(http.HandlerFunc(func(w http.ResponseWriter, r *http.Request) {
				typ := "order"
				switch tc.action.(type) {
				case *Cancel:
					typ = "cancel"
				}
				fmt.Fprintf(w, `{"status":"ok","response":{"type":%q,"data":{"statuses":%s}}}`, typ, tc.statuses)
			}))
			defer server.Close()
			c := plainClient(t, server.URL)
			err := c.Exchange(context.Background(), tc.action)
			var u *UnknownOutcomeError
			var p *PartialExecutionError
			var r *ActionRejectedError
			got := ok
			switch {
			case err == nil:
			case errors.As(err, &u):
				got = unknown
			case errors.As(err, &p):
				got = partial
			case errors.As(err, &r):
				got = rejected
			default:
				t.Fatalf("plain error: %v", err)
			}
			if got != tc.want {
				t.Fatalf("got %v (%v), want %v", got, err, tc.want)
			}
			if got == partial && len(p.Executed)+len(p.Rejected)+len(p.Tracking) != len(orderLegs(tc.action))+len(cancelLegsOf(tc.action)) {
				t.Fatalf("every leg must be accounted for: %+v", p)
			}
		})
	}
}

// plainClient is a journal-less client for the account of its own signer.
func plainClient(t *testing.T, url string) *Client {
	t.Helper()
	c, err := newTestClient(url)
	if err != nil {
		t.Fatal(err)
	}
	c.SignChainID = 12589
	return c
}

func cancelLegsOf(action any) []CancelItem {
	if c, ok := action.(*Cancel); ok {
		return c.Cancels
	}
	return nil
}

func TestTrackingLegsAreReported(t *testing.T) {
	server := httptest.NewServer(http.HandlerFunc(func(w http.ResponseWriter, r *http.Request) {
		fmt.Fprint(w, `{"status":"ok","response":{"type":"order","data":{"statuses":[{"resting":{"oid":1}},"waitingForTrigger"]}}}`)
	}))
	defer server.Close()
	c := plainClient(t, server.URL)
	err := c.Exchange(context.Background(), &Place{Type: "order", Orders: []Order{alo("100"), alo("99")}, Grouping: "na"})
	var p *PartialExecutionError
	if !errors.As(err, &p) || len(p.Tracking) != 1 || p.Tracking[0].Index != 1 || p.Tracking[0].Status != "waitingForTrigger" || len(p.Executed) != 1 || len(p.Rejected) != 0 {
		t.Fatalf("err = %v", err)
	}
}

// Finding: only the SDK's ordered action types can be signed.
func TestSigningRefusesUnorderedActions(t *testing.T) {
	c := restVaultClient(t)
	raw := json.RawMessage(`{"type":"cancel","cancels":[{"a":0,"o":1}]}`)
	var nilPlace *Place
	for name, action := range map[string]any{
		"map":             map[string]any{"type": "cancel", "cancels": []any{}},
		"json.RawMessage": raw,
		"bytes":           []byte(raw),
		"string":          string(raw),
		"nil":             nil,
		"nil pointer":     nilPlace,
		"anonymous struct": struct {
			Type string `json:"type"`
		}{"cancel"},
	} {
		t.Run(name, func(t *testing.T) {
			if _, err := c.Sign(action, 1); !errors.Is(err, errUnsupportedAction) {
				t.Errorf("Sign: %v", err)
			}
			if _, err := c.SignedRequest(action, 1); !errors.Is(err, errUnsupportedAction) {
				t.Errorf("SignedRequest: %v", err)
			}
		})
	}
	// Raw JSON goes through ParseAction, whose typed result signs and sends the same bytes.
	parsed, err := ParseAction(raw)
	if err != nil {
		t.Fatal(err)
	}
	body, err := c.SignedRequest(parsed, 1)
	if err != nil || !strings.HasPrefix(string(body), `{"action":`+string(raw)+`,"nonce":1,`) {
		t.Fatalf("body = %s, %v", body, err)
	}
	// Exchange refuses them before anything is sent, with or without a journal.
	var sent atomic.Int32
	server := httptest.NewServer(http.HandlerFunc(func(http.ResponseWriter, *http.Request) { sent.Add(1) }))
	defer server.Close()
	c.URL = server.URL
	for _, client := range []*Client{c, plainClient(t, server.URL)} {
		if err := client.Exchange(context.Background(), raw); !errors.Is(err, errUnsupportedAction) || sent.Load() != 0 {
			t.Fatalf("Exchange(json.RawMessage) = %v, sent=%d", err, sent.Load())
		}
	}
}
