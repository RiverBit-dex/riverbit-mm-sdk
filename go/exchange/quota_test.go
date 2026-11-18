package exchange

import (
	"context"
	"encoding/json"
	"errors"
	"fmt"
	"net/http"
	"net/http/httptest"
	"os"
	"path/filepath"
	"strings"
	"sync"
	"sync/atomic"
	"testing"
	"time"
)

const quotaText = "Too many cumulative requests sent (10045 > 10025) for cumulative volume traded $25.88. Place taker orders to free up 1 request per USDC traded."

func TestUserRateLimitParsesNumbersAndStrings(t *testing.T) {
	for name, body := range map[string]string{
		"numbers": `{"cumVlm":"25.88","nRequestsCap":10025,"nRequestsSurplus":0,"nRequestsUsed":45}`,
		"strings": `{"cumVlm":"25.88","nRequestsCap":"10025","nRequestsSurplus":"0","nRequestsUsed":"45"}`,
	} {
		t.Run(name, func(t *testing.T) {
			var got UserRateLimit
			if err := json.Unmarshal([]byte(body), &got); err != nil {
				t.Fatal(err)
			}
			if got.CumVlm != "25.88" || got.Cap != 10025 || got.Used != 45 || got.Surplus != 0 || got.Remaining() != 9980 {
				t.Fatalf("got %+v remaining %d", got, got.Remaining())
			}
		})
	}
	for _, bad := range []string{`{"cumVlm":"1","nRequestsCap":-1,"nRequestsSurplus":0,"nRequestsUsed":0}`, `{"cumVlm":"1","nRequestsCap":1.5,"nRequestsSurplus":0,"nRequestsUsed":0}`, `{"cumVlm":"1","nRequestsCap":1,"nRequestsUsed":0}`, `{"nRequestsCap":1,"nRequestsSurplus":0,"nRequestsUsed":0}`} {
		var got UserRateLimit
		if err := json.Unmarshal([]byte(bad), &got); err == nil {
			t.Errorf("%s accepted", bad)
		}
	}
	var big UserRateLimit
	if err := json.Unmarshal([]byte(`{"cumVlm":"0","nRequestsCap":18446744073709551615,"nRequestsSurplus":18446744073709551615,"nRequestsUsed":0}`), &big); err != nil {
		t.Fatal(err)
	}
	if big.Remaining() != ^uint64(0) {
		t.Fatalf("remaining must saturate: %d", big.Remaining())
	}
	over := UserRateLimit{Cap: 10, Used: 50, Surplus: 0}
	if over.Remaining() != 0 {
		t.Fatalf("over the cap: %d", over.Remaining())
	}
	if (UserRateLimit{Cap: 10, Used: 0, Surplus: 7}).Remaining() != 17 {
		t.Fatal("surplus is added")
	}
}

func TestUserRateLimitQueryAndUnavailableNode(t *testing.T) {
	var last atomic.Value
	status := http.StatusOK
	server := httptest.NewServer(http.HandlerFunc(func(w http.ResponseWriter, r *http.Request) {
		var body map[string]any
		_ = json.NewDecoder(r.Body).Decode(&body)
		last.Store(body)
		if status != http.StatusOK {
			http.Error(w, "not implemented", status)
			return
		}
		fmt.Fprint(w, `{"cumVlm":"25.88","nRequestsCap":10025,"nRequestsSurplus":3,"nRequestsUsed":0}`)
	}))
	defer server.Close()
	got, err := NewInfoClient(server.URL).UserRateLimit(context.Background(), testVault)
	if err != nil {
		t.Fatal(err)
	}
	if body := last.Load().(map[string]any); body["type"] != "userRateLimit" || body["user"] != testVault {
		t.Fatalf("query = %v", body)
	}
	if got.Remaining() != 10028 {
		t.Fatalf("remaining %d", got.Remaining())
	}
	c := plainClient(t, server.URL)
	if _, err := c.UserRateLimit(context.Background()); err != nil {
		t.Fatal(err)
	}
	if body := last.Load().(map[string]any); body["user"] != c.AccountAddress() {
		t.Fatalf("client queries %v", body["user"])
	}
	status = http.StatusNotImplemented
	if _, err := NewInfoClient(server.URL).UserRateLimit(context.Background(), testVault); err == nil || !strings.Contains(err.Error(), "501") {
		t.Fatalf("HTTP 501 must surface as a read error: %v", err)
	}
}

func TestReserveRequestWeightValidation(t *testing.T) {
	dest := "0x2222222222222222222222222222222222222222"
	for name, tc := range map[string]struct {
		action *ReserveRequestWeight
		ok     bool
	}{
		"one":               {NewReserveRequestWeight(1), true},
		"max":               {NewReserveRequestWeight(MaxReserveWeight), true},
		"zero":              {NewReserveRequestWeight(0), false},
		"over max":          {NewReserveRequestWeight(MaxReserveWeight + 1), false},
		"destination":       {NewReserveRequestWeightFor(5, dest), true},
		"upper destination": {NewReserveRequestWeightFor(5, strings.ToUpper(dest[:2])+dest[2:]), true}, // lowercased by the constructor
		"unnormalized":      {&ReserveRequestWeight{Type: "reserveRequestWeight", Weight: 5, Destination: "0xAbCdEf0123456789aBcDeF0123456789AbCdEf01"}, false},
		"short":             {NewReserveRequestWeightFor(5, "0x1234"), false},
		"wrong type":        {&ReserveRequestWeight{Type: "order", Weight: 5}, false},
	} {
		t.Run(name, func(t *testing.T) {
			c := plainClient(t, "http://example.invalid")
			_, err := c.Sign(tc.action, 1)
			if (err == nil) != tc.ok {
				t.Fatalf("err = %v, want ok=%t", err, tc.ok)
			}
		})
	}
	if ReserveCostMicroUSDC(1000) != 500000 || NewReserveRequestWeight(2).CostMicroUSDC() != 1000 || ReserveCostMicroUSDC(^uint64(0)) != ^uint64(0) {
		t.Fatal("cost helper")
	}
	// ParseAction round-trips and weight must be an integer JSON number.
	a, err := ParseAction([]byte(`{"type":"reserveRequestWeight","weight":7}`))
	if err != nil || a.(*ReserveRequestWeight).Weight != 7 {
		t.Fatalf("%v %v", a, err)
	}
	if _, err := ParseAction([]byte(`{"type":"reserveRequestWeight","weight":"7"}`)); err == nil {
		t.Fatal("string weight accepted")
	}
	if _, err := ParseAction([]byte(`{"type":"reserveRequestWeight","weight":7.5}`)); err == nil {
		t.Fatal("fractional weight accepted")
	}
}

func TestReserveRequestWeightDestinationOnVaultClient(t *testing.T) {
	c, err := newTestVaultClient("http://example.invalid", testVault)
	if err != nil {
		t.Fatal(err)
	}
	c.SignChainID = 12589
	if _, err := c.Sign(NewReserveRequestWeight(10), 1); err != nil {
		t.Fatalf("vault pays: %v", err)
	}
	if _, err := c.Sign(NewReserveRequestWeightFor(10, testVault), 1); err != nil {
		t.Fatalf("destination equal to the vault: %v", err)
	}
	if _, err := c.Sign(NewReserveRequestWeightFor(10, "0x2222222222222222222222222222222222222222"), 1); err == nil {
		t.Fatal("a vault client must not buy for another account")
	}
	// The master pays for the vault: a client without a vault and a destination.
	master := plainClient(t, "http://example.invalid")
	body, err := master.SignedRequest(NewReserveRequestWeightFor(10, testVault), 1)
	if err != nil || strings.Contains(string(body), "vaultAddress") || !strings.Contains(string(body), `"destination":"`+testVault+`"`) {
		t.Fatalf("%s %v", body, err)
	}
}

func quotaServer(t *testing.T, reply string) (*httptest.Server, *atomic.Int32) {
	t.Helper()
	var sends atomic.Int32
	server := httptest.NewServer(http.HandlerFunc(func(w http.ResponseWriter, r *http.Request) {
		if r.URL.Path == "/exchange" {
			sends.Add(1)
			fmt.Fprint(w, reply)
			return
		}
		fmt.Fprint(w, `{"height":6,"status":"order","order":{"status":"canceled","order":{"oid":5}}}`)
	}))
	t.Cleanup(server.Close)
	return server, &sends
}

func TestQuotaRejectedLegsAreFinalAndDistinguishable(t *testing.T) {
	wrapped := "action rejected by execution (code 1800) (" + quotaText + ")"
	partialReply := fmt.Sprintf(`{"status":"ok","response":{"type":"order","data":{"statuses":[{"resting":{"oid":5}},{"error":%q},{"error":%q}]}}}`, wrapped, wrapped)
	allReply := fmt.Sprintf(`{"status":"ok","response":{"type":"order","data":{"statuses":[{"error":%q},{"error":%q}]}}}`, wrapped, "action rejected at ingress (code 1800) ("+quotaText+")")
	for _, durable := range []bool{true, false} {
		t.Run(fmt.Sprintf("partial durable=%t", durable), func(t *testing.T) {
			server, _ := quotaServer(t, partialReply)
			c := plainClient(t, server.URL)
			if durable {
				c = restVaultClient(t)
				c.URL = server.URL
			}
			err := c.Exchange(context.Background(), &Place{Type: "order", Orders: []Order{alo("1"), alo("2"), alo("3")}, Grouping: "na"})
			var partial *PartialExecutionError
			if !errors.As(err, &partial) || !partial.IsQuotaRejection() || !IsQuotaRejection(err) || !errors.Is(err, ErrRequestQuotaExceeded) {
				t.Fatalf("err = %v", err)
			}
			var unknown *UnknownOutcomeError
			if errors.As(err, &unknown) {
				t.Fatal("a quota rejection is not an unknown outcome")
			}
			if len(partial.Executed) != 1 || len(partial.Rejected) != 2 || !partial.Rejected[0].IsQuotaRejection() || !strings.Contains(partial.Rejected[1].Error, "code 1800") {
				t.Fatalf("legs %+v", partial)
			}
			if c.LastReceipt == nil || !c.LastReceipt.IsQuotaRejection() || !c.LastReceipt.Legs[1].IsQuotaRejection() || c.LastReceipt.Legs[0].IsQuotaRejection() {
				t.Fatalf("receipt %+v", c.LastReceipt)
			}
		})
		t.Run(fmt.Sprintf("every leg durable=%t", durable), func(t *testing.T) {
			server, _ := quotaServer(t, allReply)
			c := plainClient(t, server.URL)
			if durable {
				c = restVaultClient(t)
				c.URL = server.URL
			}
			err := c.Exchange(context.Background(), &Place{Type: "order", Orders: []Order{alo("1"), alo("2")}, Grouping: "na"})
			if !IsQuotaRejection(err) {
				t.Fatalf("err = %v", err)
			}
			if durable != errors.Is(err, ErrExecutionRejected) {
				t.Fatalf("durable=%t err = %v", durable, err)
			}
			var rejected *ActionRejectedError
			if !durable && (!errors.As(err, &rejected) || !rejected.IsQuotaRejection()) {
				t.Fatalf("err = %v", err)
			}
			if durable {
				if _, statErr := os.Stat(c.pendingPath()); !os.IsNotExist(statErr) {
					t.Fatalf("journal remains: %v", statErr)
				}
			}
		})
	}
}

func TestQuotaRejectedCancelLegsAreFinal(t *testing.T) {
	reply := fmt.Sprintf(`{"status":"ok","response":{"type":"cancel","data":{"statuses":["success",{"error":%q}]}}}`, "action rejected by execution (code 1800) ("+quotaText+" asset=3)")
	server, _ := quotaServer(t, reply)
	c := restVaultClient(t)
	c.URL = server.URL
	c.RequireExecutionHeight = false
	ctx, cancel := context.WithTimeout(context.Background(), 5*time.Second)
	defer cancel()
	err := c.Exchange(ctx, NewCancel(CancelItem{Asset: 3, OID: 5}, CancelItem{Asset: 3, OID: 6}))
	var partial *PartialExecutionError
	if !errors.As(err, &partial) || !IsQuotaRejection(err) {
		t.Fatalf("err = %v", err)
	}
	if len(partial.Executed) != 1 || len(partial.Rejected) != 1 || partial.Rejected[0].Index != 1 {
		t.Fatalf("%+v", partial)
	}
}

func TestQuotaRejectedModifyWithCommittedResultIsFinal(t *testing.T) {
	text := "action rejected by execution (code 1800) (" + quotaText + ")"
	for name, reply := range map[string]string{
		"plain text":       fmt.Sprintf(`{"status":"err","response":%q}`, quotaText),
		"wrapped":          fmt.Sprintf(`{"status":"err","response":%q}`, text),
		"ingress":          fmt.Sprintf(`{"status":"err","response":%q}`, "action rejected at ingress (code 1800) ("+quotaText+")"),
		"committed result": fmt.Sprintf(`{"status":"err","response":%q,"committedResult":{"type":"order","data":{"statuses":[{"localCommitted":true}]}}}`, quotaText),
	} {
		for _, durable := range []bool{true, false} {
			t.Run(fmt.Sprintf("%s durable=%t", name, durable), func(t *testing.T) {
				server, _ := quotaServer(t, reply)
				c := plainClient(t, server.URL)
				if durable {
					c = restVaultClient(t)
					c.URL = server.URL
				}
				err := c.Exchange(context.Background(), NewModify(9, alo("1")))
				var unknown *UnknownOutcomeError
				if errors.As(err, &unknown) || !IsQuotaRejection(err) {
					t.Fatalf("err = %v", err)
				}
				if durable && !errors.Is(err, ErrExecutionRejected) {
					t.Fatalf("durable: %v", err)
				}
				if !c.LastReceipt.IsQuotaRejection() {
					t.Fatal("receipt does not flag the quota rejection")
				}
			})
		}
	}
	if IsQuotaText("Price must be divisible by tick size.") || IsQuotaRejection(errors.New("x")) || IsQuotaRejection(nil) {
		t.Fatal("false positive")
	}
}

func reserveClient(t *testing.T, url string) *Client {
	c := restVaultClient(t)
	c.URL = url
	return c
}

func TestReservePurchaseOutcomes(t *testing.T) {
	overflow := "action rejected by execution (code 1801) (reserved request weight would overflow)"
	for name, tc := range map[string]struct {
		reply string
		want  string // "ok", "rejected" or "unknown"
		code  int
	}{
		"ok":              {`{"status":"ok","response":{"type":"default"}}`, "ok", 0},
		"err":             {`{"status":"err","response":"insufficient balance"}`, "rejected", 0},
		"overflow err":    {fmt.Sprintf(`{"status":"err","response":%q}`, overflow), "rejected", CodeReserveWeightOverflow},
		"overflow leg":    {fmt.Sprintf(`{"status":"ok","response":{"type":"order","data":{"statuses":[{"error":%q}]}}}`, overflow), "rejected", CodeReserveWeightOverflow},
		"overflow in err": {fmt.Sprintf(`{"status":"err","response":{"type":"order","data":{"statuses":[{"error":%q}]}}}`, overflow), "rejected", CodeReserveWeightOverflow},
		"duplicate nonce": {`{"status":"err","response":"action rejected by execution (code 205): duplicate nonce"}`, "unknown", 0},
		"empty ok":        {`{"status":"ok","response":{}}`, "unknown", 0},
		"extra member":    {`{"status":"ok","response":{"type":"default","x":1}}`, "unknown", 0},
		"other status":    {`{"status":"weird","response":"?"}`, "unknown", 0},
		"not json":        {`<html>`, "unknown", 0},
	} {
		for _, durable := range []bool{true, false} {
			t.Run(fmt.Sprintf("%s durable=%t", name, durable), func(t *testing.T) {
				server, sends := quotaServer(t, tc.reply)
				c := plainClient(t, server.URL)
				if durable {
					c = reserveClient(t, server.URL)
				}
				ctx, cancel := context.WithTimeout(context.Background(), 2*time.Second)
				defer cancel()
				err := c.Exchange(ctx, NewReserveRequestWeight(1000))
				var unknown *UnknownOutcomeError
				var rejected *ActionRejectedError
				switch tc.want {
				case "ok":
					if err != nil {
						t.Fatal(err)
					}
				case "rejected":
					if durable && !errors.Is(err, ErrExecutionRejected) {
						t.Fatalf("err = %v", err)
					}
					if !durable && (!errors.As(err, &rejected) || rejected.Code != tc.code) {
						t.Fatalf("err = %v", err)
					}
					if errors.As(err, &unknown) {
						t.Fatal("rejection is final, not unknown")
					}
				case "unknown":
					if !errors.As(err, &unknown) || unknown.Nonce == 0 || len(unknown.Cloids) != 0 || !strings.Contains(string(unknown.Action), `"weight":1000`) {
						t.Fatalf("err = %v", err)
					}
				}
				if sends.Load() != 1 {
					t.Fatalf("%d sends, want exactly 1", sends.Load())
				}
				if durable {
					_, statErr := os.Stat(c.pendingPath())
					if pending := !os.IsNotExist(statErr); pending != (tc.want == "unknown") {
						t.Fatalf("journal present = %t for %s", pending, tc.want)
					}
				}
			})
		}
	}
}

func TestPendingPurchaseStaysPendingAndIsNeverResent(t *testing.T) {
	var mu sync.Mutex
	var bodies []string
	var infos int
	server := httptest.NewServer(http.HandlerFunc(func(w http.ResponseWriter, r *http.Request) {
		mu.Lock()
		defer mu.Unlock()
		if r.URL.Path == "/exchange" {
			b := make([]byte, 4096)
			n, _ := r.Body.Read(b)
			bodies = append(bodies, string(b[:n]))
			http.Error(w, "reply lost", http.StatusBadGateway)
			return
		}
		infos++
		http.Error(w, "unexpected", http.StatusInternalServerError)
	}))
	defer server.Close()
	c := reserveClient(t, server.URL)
	err := c.Exchange(context.Background(), NewReserveRequestWeight(250000))
	var unknown *UnknownOutcomeError
	if !errors.As(err, &unknown) || !errors.Is(err, ErrPurchaseUnresolved) || !strings.Contains(err.Error(), "userRateLimit") {
		t.Fatalf("err = %v", err)
	}
	nonce := unknown.Nonce
	// Recovery, also from a fresh client on the same directory (a restart),
	// keeps it pending and sends and queries nothing.
	restarted := reserveClient(t, server.URL)
	restarted.StateDir = c.StateDir
	for _, rc := range []*Client{c, restarted} {
		err = rc.RecoverPending(context.Background())
		if !errors.As(err, &unknown) || unknown.Nonce != nonce || !errors.Is(err, ErrPurchaseUnresolved) {
			t.Fatalf("recover: %v", err)
		}
	}
	if err = c.Exchange(context.Background(), NewReserveRequestWeight(1)); !errors.Is(err, ErrPendingUnresolved) {
		t.Fatalf("a new request must be refused: %v", err)
	}
	mu.Lock()
	if len(bodies) != 1 || infos != 0 {
		t.Fatalf("sends=%d info queries=%d; want 1 and 0", len(bodies), infos)
	}
	mu.Unlock()
	// The operator acknowledges after comparing userRateLimit.
	if err := c.AcknowledgePendingPurchase(nonce+1, "x"); err == nil {
		t.Fatal("wrong nonce accepted")
	}
	if err := c.AcknowledgePendingPurchase(nonce, "compared userRateLimit before and after: not applied"); err != nil {
		t.Fatal(err)
	}
	if err := c.RecoverPending(context.Background()); err != nil {
		t.Fatalf("after acknowledge: %v", err)
	}
	audit, err := os.ReadFile(filepath.Join(c.StateDir, c.journalIdentity()+".audit.jsonl"))
	if err != nil || !strings.Contains(string(audit), `"event":"purchase_acknowledged"`) {
		t.Fatalf("audit: %v\n%s", err, audit)
	}
	// An order journal cannot be acknowledged as a purchase.
	if err := c.AcknowledgePendingPurchase(nonce, ""); err == nil {
		t.Fatal("nothing pending, must fail")
	}
}

func TestPurchaseWithoutChainIDIsNotJournaled(t *testing.T) {
	c := reserveClient(t, "http://example.invalid")
	c.SignChainID = 0
	if err := c.Exchange(context.Background(), NewReserveRequestWeight(5)); !errors.Is(err, ErrNoChainID) {
		t.Fatal(err)
	}
	if err := c.Exchange(context.Background(), NewReserveRequestWeight(0)); err == nil {
		t.Fatal("zero weight")
	}
	if _, statErr := os.Stat(c.pendingPath()); !os.IsNotExist(statErr) {
		t.Fatal("a rejected-before-signing purchase was journaled")
	}
}

func TestGatewayCredentialHeader(t *testing.T) {
	const secret = "mm-credential-0123456789"
	var mu sync.Mutex
	seen := map[string]string{}
	server := httptest.NewServer(http.HandlerFunc(func(w http.ResponseWriter, r *http.Request) {
		mu.Lock()
		seen[r.URL.Path] = r.Header.Get(GatewayCredentialHeader)
		mu.Unlock()
		if r.URL.Path == "/exchange" {
			fmt.Fprint(w, `{"status":"ok","response":{"type":"default"}}`)
			return
		}
		fmt.Fprint(w, `{"nextAgentChainId":12589}`)
	}))
	defer server.Close()
	get := func(path string) string { mu.Lock(); defer mu.Unlock(); return seen[path] }

	c := plainClient(t, server.URL)
	if err := c.LoadChainID(context.Background()); err != nil || get("/info") != "" {
		t.Fatalf("unset: %v %q", err, get("/info"))
	}
	if err := c.Exchange(context.Background(), NewReserveRequestWeight(3)); err != nil || get("/exchange") != "" {
		t.Fatalf("unset: %v %q", err, get("/exchange"))
	}
	if err := c.SetGatewayCredential(secret); err != nil {
		t.Fatal(err)
	}
	if err := c.LoadChainID(context.Background()); err != nil || get("/info") != secret {
		t.Fatalf("/info header %q: %v", get("/info"), err)
	}
	if err := c.Exchange(context.Background(), NewReserveRequestWeight(3)); err != nil || get("/exchange") != secret {
		t.Fatalf("/exchange header %q: %v", get("/exchange"), err)
	}
	// Empty clears it.
	if err := c.SetGatewayCredential(""); err != nil {
		t.Fatal(err)
	}
	seen = map[string]string{}
	_ = c.LoadChainID(context.Background())
	if get("/info") != "" {
		t.Fatal("cleared credential still sent")
	}
	info := NewInfoClient(server.URL)
	if err := info.SetGatewayCredential(secret); err != nil {
		t.Fatal(err)
	}
	if _, err := info.ChainIdentity(context.Background()); err != nil || get("/info") != secret {
		t.Fatalf("InfoClient header %q: %v", get("/info"), err)
	}
}

func TestGatewayCredentialRejectsInvalidHeaderValues(t *testing.T) {
	c := plainClient(t, "http://example.invalid")
	info := NewInfoClient("http://example.invalid")
	for _, bad := range []string{"abc\r\nX-Evil: 1", "abc\n", "abc\r", "café", "tab\there", "\x00", "del\x7f"} {
		if err := c.SetGatewayCredential(bad); err == nil || strings.Contains(err.Error(), "abc") {
			t.Errorf("Client accepted or echoed %q: %v", bad, err)
		}
		if err := info.SetGatewayCredential(bad); err == nil {
			t.Errorf("InfoClient accepted %q", bad)
		}
	}
	if c.token != nil || info.token != nil {
		t.Fatal("a refused credential was stored")
	}
}

// The credential never reaches another URL: a redirect is not followed, so the
// header cannot follow it.
func TestGatewayCredentialIsNotSentAcrossRedirects(t *testing.T) {
	var hits atomic.Int32
	target := httptest.NewServer(http.HandlerFunc(func(w http.ResponseWriter, r *http.Request) {
		hits.Add(1)
		fmt.Fprint(w, `{}`)
	}))
	defer target.Close()
	redirector := httptest.NewServer(http.HandlerFunc(func(w http.ResponseWriter, r *http.Request) {
		http.Redirect(w, r, target.URL+r.URL.Path, http.StatusTemporaryRedirect)
	}))
	defer redirector.Close()
	c := plainClient(t, redirector.URL)
	c.HTTP = &http.Client{Timeout: time.Second} // a caller client that would follow
	if err := c.SetGatewayCredential("secret-credential"); err != nil {
		t.Fatal(err)
	}
	_ = c.Exchange(context.Background(), NewReserveRequestWeight(1))
	var out json.RawMessage
	_ = c.Info(context.Background(), map[string]string{"type": "meta"}, &out)
	if hits.Load() != 0 {
		t.Fatalf("redirect target was reached %d times", hits.Load())
	}
}

// Like the signer, the credential is hidden from every printed form, from error
// messages and from the journal files.
func TestGatewayCredentialIsNeverPrintedLoggedOrJournaled(t *testing.T) {
	const secret = "SECRET-credential-value-42"
	server := httptest.NewServer(http.HandlerFunc(func(w http.ResponseWriter, r *http.Request) {
		if r.URL.Path == "/exchange" {
			http.Error(w, "reply lost", http.StatusBadGateway)
			return
		}
		http.Error(w, "boom", http.StatusInternalServerError)
	}))
	defer server.Close()
	var logs strings.Builder
	c := reserveClient(t, server.URL)
	c.Logger = newBufferLogger(&logs)
	if err := c.SetGatewayCredential(secret); err != nil {
		t.Fatal(err)
	}
	info := c.InfoClient()
	var texts []string
	texts = append(texts, fmt.Sprintf("%v %+v %#v", c, c, c), fmt.Sprintf("%v %+v %#v", info, info, info), fmt.Sprintf("%v %+v %#v", c.token, c.token, c.token))
	_, err := c.UserRateLimit(context.Background())
	texts = append(texts, fmt.Sprint(err))
	err = c.Exchange(context.Background(), NewReserveRequestWeight(10))
	texts = append(texts, fmt.Sprint(err))
	var unknown *UnknownOutcomeError
	if errors.As(err, &unknown) {
		texts = append(texts, fmt.Sprintf("%+v", unknown), string(unknown.Action))
	}
	err = c.RecoverPending(context.Background())
	texts = append(texts, fmt.Sprint(err), logs.String())
	for _, text := range texts {
		if strings.Contains(text, secret) {
			t.Fatalf("credential leaked: %s", text)
		}
	}
	entries, err := os.ReadDir(c.StateDir)
	if err != nil || len(entries) == 0 {
		t.Fatalf("state dir: %v %d", err, len(entries))
	}
	for _, e := range entries {
		raw, err := os.ReadFile(filepath.Join(c.StateDir, e.Name()))
		if err != nil {
			t.Fatal(err)
		}
		if strings.Contains(string(raw), secret) {
			t.Fatalf("credential in journal file %s", e.Name())
		}
	}
}
