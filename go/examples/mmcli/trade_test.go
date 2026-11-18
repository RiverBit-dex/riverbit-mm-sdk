package main

import (
	"context"
	"encoding/json"
	"errors"
	"fmt"
	"io"
	"log/slog"
	"net/http"
	"net/http/httptest"
	"os"
	"path/filepath"
	"strings"
	"sync"
	"testing"
	"time"

	"github.com/ethereum/go-ethereum/crypto"
	"github.com/riverbit-dex/riverbit-mm-sdk/go/binws"
	"github.com/riverbit-dex/riverbit-mm-sdk/go/binws/binwstest"
	"github.com/riverbit-dex/riverbit-mm-sdk/go/exchange"
)

func TestExampleActionFilesParse(t *testing.T) {
	files, err := filepath.Glob("actions/*.json")
	if err != nil || len(files) == 0 {
		t.Fatalf("examples: %v %v", files, err)
	}
	actions, err := readActions(strings.Join(files, ","))
	if err != nil || len(actions) != len(files) {
		t.Fatalf("readActions = %d, %v", len(actions), err)
	}
	for _, a := range actions {
		if err := validateActionAsset(a, 1); err != nil {
			t.Errorf("%T: %v", a, err)
		}
	}
}

const (
	testVault  = "0x00000000000000000000000000000000000000aa"
	testLeader = "0x00000000000000000000000000000000000000bb"
	testKeyHex = "0000000000000000000000000000000000000000000000000000000000000007"
)

func testSigner() string {
	key, _ := crypto.HexToECDSA(testKeyHex)
	return strings.ToLower(crypto.PubkeyToAddress(key.PublicKey).Hex())
}

// fakeGateway is the HTTP side: POST /info (startup and reconciliation
// queries) and POST /exchange.
type fakeGateway struct {
	*httptest.Server
	mu                  sync.Mutex
	infos               []string // info types in arrival order
	exchanges           []string // raw /exchange bodies
	unknown             bool
	orders              map[string]exchange.Order // order legs received, by cloid
	orderStatusObserved chan struct{}
}

func newFakeGateway(t *testing.T, account string) *fakeGateway {
	g := &fakeGateway{orderStatusObserved: make(chan struct{}, 1)}
	signer := testSigner()
	g.Server = httptest.NewServer(http.HandlerFunc(func(w http.ResponseWriter, r *http.Request) {
		body, _ := io.ReadAll(r.Body)
		if r.URL.Path == "/exchange" {
			var req struct {
				Action exchange.Place `json:"action"`
			}
			_ = json.Unmarshal(body, &req)
			g.mu.Lock()
			g.exchanges = append(g.exchanges, string(body))
			if g.orders == nil {
				g.orders = map[string]exchange.Order{}
			}
			legs := make([]string, len(req.Action.Orders))
			for i, o := range req.Action.Orders {
				g.orders[o.Cloid] = o
				legs[i] = fmt.Sprintf(`{"resting":{"oid":%d}}`, 501+i) // one status per submitted leg
			}
			g.mu.Unlock()
			fmt.Fprintf(w, `{"status":"ok","response":{"type":"order","data":{"statuses":[%s]}}}`, strings.Join(legs, ","))
			return
		}
		if r.URL.Path != "/info" {
			t.Errorf("HTTP path %s, want /info or /exchange (no /api prefix)", r.URL.Path)
		}
		var q map[string]any
		_ = json.Unmarshal(body, &q)
		typ, _ := q["type"].(string)
		g.mu.Lock()
		g.infos = append(g.infos, typ)
		g.mu.Unlock()
		switch typ {
		case "chainIdentity":
			fmt.Fprint(w, `{"agentChainId":12581,"chainId":12581,"nextAgentChainId":12581,"nextChainId":12581,"signatureChainId":"0x3125"}`)
		case "vaultRegistry":
			fmt.Fprintf(w, `{"vaultAddress":%q,"status":"normal","class":"guild"}`, testVault)
		case "vaultDetails":
			fmt.Fprintf(w, `{"leader":%q}`, testLeader)
		case "userRole":
			if q["user"] != signer {
				t.Errorf("userRole user = %v, want the API wallet %s", q["user"], signer)
			}
			fmt.Fprintf(w, `{"role":"agent","data":{"user":%q}}`, testLeader)
		case "metaAndAssetCtxs":
			fmt.Fprint(w, `[{"universe":[{"name":"RBT","szDecimals":2,"maxLeverage":20},{"name":"BTC","szDecimals":5,"maxLeverage":40}]},[]]`)
		case "orderStatus", "openOrders":
			if q["user"] != account {
				t.Errorf("%s user = %v, want %s", typ, q["user"], account)
			}
			if typ == "openOrders" {
				fmt.Fprint(w, `[]`)
				return
			}
			g.mu.Lock()
			unknown := g.unknown
			g.mu.Unlock()
			if unknown {
				fmt.Fprint(w, `{"height":3,"status":"unknownOid"}`)
				select {
				case g.orderStatusObserved <- struct{}{}:
				default:
				}
				return
			}
			if cloid, ok := q["oid"].(string); ok { // a lookup by cloid must find an order tied to the request
				g.mu.Lock()
				o := g.orders[cloid]
				g.mu.Unlock()
				side := "A"
				if o.Buy {
					side = "B"
				}
				fmt.Fprintf(w, `{"height":3,"status":"order","order":{"status":"open","order":{"coin":"BTC","oid":501,"side":%q,"limitPx":%q,"sz":%q,"timestamp":%d,"cloid":%q}}}`, side, o.Price, o.Size, time.Now().UnixMilli(), cloid)
				return
			}
			oid, _ := q["oid"].(float64) // the receipt names the order IDs
			fmt.Fprintf(w, `{"height":3,"status":"order","order":{"status":"open","order":{"coin":"BTC","oid":%d}}}`, uint64(oid))
		default:
			t.Errorf("unexpected info %s", body)
		}
	}))
	t.Cleanup(g.Close)
	return g
}

// recover reconnects using the same identity but never reads ACTION_FILE or
// submits another exchange. It works for a vault and an API wallet's master.
func TestRecoverCommandNeverSubmitsActions(t *testing.T) {
	for _, vault := range []string{"", testVault} {
		t.Run("vault="+vault, func(t *testing.T) {
			account := testLeader
			if vault != "" {
				account = vault
			}
			gateway := newFakeGateway(t, account)
			gateway.unknown = true
			cfg := tradeConfig(t, "ws://127.0.0.1:1/bin-ws", gateway.URL, vault)
			c, err := newClient(cfg, vault)
			if err != nil {
				t.Fatal(err)
			}
			c.StateDir, c.SignChainID = cfg.StateDir, 12581
			if vault == "" {
				if err := c.SetPrincipal(testLeader); err != nil {
					t.Fatal(err)
				}
			}
			// Cancel only after submission and the first reconciliation query.
			// A short wall-clock deadline can expire before HTTP sends anything
			// on a loaded race-test runner, leaving no submitted request to recover.
			ctx, cancel := context.WithTimeout(context.Background(), 10*time.Second)
			defer cancel()
			submitted := make(chan error, 1)
			go func() {
				submitted <- c.Exchange(ctx, &exchange.Place{Type: "order", Orders: []exchange.Order{{Asset: 0, Price: "100", Size: "1", Type: exchange.OrderType{Limit: exchange.Limit{TIF: "Alo"}}}}, Grouping: "na"})
			}()
			select {
			case <-gateway.orderStatusObserved:
				cancel()
			case <-ctx.Done():
				t.Fatalf("initial request never reached reconciliation: %v", ctx.Err())
			}
			if err = <-submitted; !errors.Is(err, context.Canceled) {
				t.Fatalf("prepare pending: %v", err)
			}
			gateway.mu.Lock()
			gateway.unknown = false
			gateway.mu.Unlock()
			t.Setenv("ACTION_FILE", "/does-not-exist.json")
			ctx, cancel = context.WithTimeout(context.Background(), time.Second)
			err = runTrade(ctx, cfg, slog.New(slog.NewTextHandler(io.Discard, nil)), true)
			cancel()
			if err != nil {
				t.Fatal(err)
			}
			gateway.mu.Lock()
			count := len(gateway.exchanges)
			gateway.mu.Unlock()
			if count != 1 {
				t.Fatalf("recovery submitted actions: /exchange requests = %d, want 1", count)
			}
			files, err := filepath.Glob(filepath.Join(cfg.StateDir, "*.pending.json"))
			if err != nil || len(files) != 0 {
				t.Fatalf("pending after recovery: %v, %v", files, err)
			}
		})
	}
}

func tradeConfig(t *testing.T, binWSURL, gatewayURL, vault string) Config {
	dir := t.TempDir()
	keyFile := filepath.Join(dir, "key")
	if err := os.WriteFile(keyFile, []byte(testKeyHex+"\n"), 0600); err != nil {
		t.Fatal(err)
	}
	t.Setenv("ACTION_FILE", "actions/btc-batch-order.sample.json")
	return Config{
		BinWSURL: binWSURL, BinWSLoginHost: "pre-api.riverbit.io", APIGatewayURL: gatewayURL,
		Coin: "BTC", BookStream: "l2", GuildVault: vault, KeyFile: keyFile, StateDir: filepath.Join(dir, "state"),
	}
}

type signedPlace struct {
	Action       exchange.Place     `json:"action"`
	Nonce        uint64             `json:"nonce"`
	Signature    exchange.Signature `json:"signature"`
	VaultAddress string             `json:"vaultAddress"`
}

// The whole `trade` command for a vault: no bin-ws connection. The signed
// order is POSTed to /exchange and reconciled with orderStatus and openOrders
// over POST /info.
func TestTradeForVault(t *testing.T) {
	ws := binwstest.New(t, "pre-api.riverbit.io")
	ws.Handle = func(*binwstest.Conn, binws.ClientFrame) { t.Error("trade must not send bin-ws frames") }
	gateway := newFakeGateway(t, testVault)
	cfg := tradeConfig(t, ws.URL(), gateway.URL, testVault)
	logger := slog.New(slog.NewTextHandler(io.Discard, nil))
	if err := trade(context.Background(), cfg, logger); err != nil {
		t.Fatal(err)
	}
	if n := len(ws.Logins()); n != 0 {
		t.Fatalf("trade dialed bin-ws (%d logins)", n)
	}
	gateway.mu.Lock()
	defer gateway.mu.Unlock()
	if len(gateway.exchanges) != 1 {
		t.Fatalf("/exchange requests = %d, want 1", len(gateway.exchanges))
	}
	var req signedPlace
	if err := json.Unmarshal([]byte(gateway.exchanges[0]), &req); err != nil {
		t.Fatal(err)
	}
	if req.VaultAddress != testVault || len(req.Action.Orders) != 2 || req.Signature.R == "" {
		t.Fatalf("request = %+v", req)
	}
	if !strings.HasPrefix(gateway.exchanges[0], `{"action":{"type":"order","orders":[`) {
		t.Fatalf("/exchange body not in signing key order: %s", gateway.exchanges[0])
	}
	got := strings.Join(gateway.infos, ",")
	if want := "chainIdentity,vaultRegistry,vaultDetails,userRole,metaAndAssetCtxs,orderStatus,orderStatus,openOrders"; got != want {
		t.Fatalf("/info sequence = %s, want %s", got, want)
	}
}

// Without a vault: the API wallet's master account comes from
// POST /info userRole instead of the bin-ws login.
func TestTradeForAPIWalletResolvesMasterViaUserRole(t *testing.T) {
	gateway := newFakeGateway(t, testLeader)
	// BinWSURL points nowhere: any dial attempt would fail the command.
	cfg := tradeConfig(t, "ws://127.0.0.1:1/bin-ws", gateway.URL, "")
	logger := slog.New(slog.NewTextHandler(io.Discard, nil))
	if err := trade(context.Background(), cfg, logger); err != nil {
		t.Fatal(err)
	}
	gateway.mu.Lock()
	defer gateway.mu.Unlock()
	var req struct {
		VaultAddress *string `json:"vaultAddress"`
	}
	if len(gateway.exchanges) != 1 || json.Unmarshal([]byte(gateway.exchanges[0]), &req) != nil || req.VaultAddress != nil {
		t.Fatalf("/exchange = %v; an API wallet trading for its master carries no vaultAddress", gateway.exchanges)
	}
	if got := strings.Join(gateway.infos, ","); got != "chainIdentity,metaAndAssetCtxs,userRole,orderStatus,orderStatus,openOrders" {
		t.Fatalf("/info sequence = %s", got)
	}
}
