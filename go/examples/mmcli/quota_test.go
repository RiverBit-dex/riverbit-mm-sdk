package main

import (
	"bytes"
	"context"
	"encoding/json"
	"fmt"
	"log/slog"
	"net/http"
	"net/http/httptest"
	"os"
	"path/filepath"
	"strings"
	"sync"
	"testing"
)

const quotaTestKey = "0000000000000000000000000000000000000000000000000000000000000001"

type quotaGateway struct {
	*httptest.Server
	mu        sync.Mutex
	exchanges int
	tokens    []string
	surplus   int
}

func newQuotaGateway(t *testing.T) *quotaGateway {
	g := &quotaGateway{}
	g.Server = httptest.NewServer(http.HandlerFunc(func(w http.ResponseWriter, r *http.Request) {
		g.mu.Lock()
		defer g.mu.Unlock()
		g.tokens = append(g.tokens, r.Header.Get("X-RiverBit-MM-Token"))
		if r.URL.Path == "/exchange" {
			g.exchanges++
			g.surplus += 500
			fmt.Fprint(w, `{"status":"ok","response":{"type":"default"}}`)
			return
		}
		var q struct {
			Type string `json:"type"`
		}
		_ = json.NewDecoder(r.Body).Decode(&q)
		switch q.Type {
		case "userRole":
			fmt.Fprint(w, `{"role":"user","data":{}}`)
		case "chainIdentity":
			fmt.Fprint(w, `{"nextAgentChainId":12589}`)
		case "userRateLimit":
			fmt.Fprintf(w, `{"cumVlm":"25.88","nRequestsCap":10025,"nRequestsSurplus":%d,"nRequestsUsed":45}`, g.surplus)
		default:
			http.Error(w, "unexpected "+q.Type, http.StatusBadRequest)
		}
	}))
	t.Cleanup(g.Close)
	return g
}

func quotaConfig(t *testing.T, g *quotaGateway) Config {
	dir := t.TempDir()
	keyFile := filepath.Join(dir, "key")
	if err := os.WriteFile(keyFile, []byte(quotaTestKey), 0o600); err != nil {
		t.Fatal(err)
	}
	return Config{APIGatewayURL: g.URL, KeyFile: keyFile, StateDir: filepath.Join(dir, "state"), GatewayCredential: "example-credential"}
}

func TestQuotaWithoutBuyRequestsSendsNothingSigned(t *testing.T) {
	g := newQuotaGateway(t)
	var out bytes.Buffer
	if err := runQuota(context.Background(), quotaConfig(t, g), slog.New(slog.DiscardHandler), &out); err != nil {
		t.Fatal(err)
	}
	if g.exchanges != 0 {
		t.Fatalf("%d signed requests sent", g.exchanges)
	}
	if !strings.Contains(out.String(), "cumVlm=25.88 cap=10025 used=45 surplus=0 remaining=9980") || !strings.Contains(out.String(), "Nothing was sent") {
		t.Fatalf("output:\n%s", out.String())
	}
	for _, token := range g.tokens {
		if token != "example-credential" {
			t.Fatalf("token header %q", token)
		}
	}
	if strings.Contains(out.String(), "example-credential") {
		t.Fatal("credential printed")
	}
}

func TestQuotaBuysOnlyWithBuyRequests(t *testing.T) {
	g := newQuotaGateway(t)
	cfg := quotaConfig(t, g)
	cfg.BuyRequests = "1000"
	var out bytes.Buffer
	if err := runQuota(context.Background(), cfg, slog.New(slog.DiscardHandler), &out); err != nil {
		t.Fatalf("%v\n%s", err, out.String())
	}
	if g.exchanges != 1 {
		t.Fatalf("%d purchases", g.exchanges)
	}
	for _, want := range []string{"Buying 1000 requests for 500000 micro-USDC", `Receipt: {"status":"ok"`, "Purchase accepted", "Quota after", "surplus=500", "9980 before, 10480 after"} {
		if !strings.Contains(out.String(), want) {
			t.Fatalf("missing %q in:\n%s", want, out.String())
		}
	}
}

func TestQuotaRejectsBadBuyRequestsBeforeAnyRequest(t *testing.T) {
	g := newQuotaGateway(t)
	for _, bad := range []string{"0", "-5", "abc", "1.5", "1000000001"} {
		cfg := quotaConfig(t, g)
		cfg.BuyRequests = bad
		if err := runQuota(context.Background(), cfg, slog.New(slog.DiscardHandler), &bytes.Buffer{}); err == nil {
			t.Errorf("BUY_REQUESTS=%q accepted", bad)
		}
	}
	if len(g.tokens) != 0 {
		t.Fatalf("%d requests sent", len(g.tokens))
	}
}

func TestInvalidCredentialIsRefusedWithoutEchoingIt(t *testing.T) {
	g := newQuotaGateway(t)
	cfg := quotaConfig(t, g)
	cfg.GatewayCredential = "secret\r\nX-Evil: 1"
	err := runQuota(context.Background(), cfg, slog.New(slog.DiscardHandler), &bytes.Buffer{})
	if err == nil || strings.Contains(err.Error(), "secret") {
		t.Fatalf("err = %v", err)
	}
}
