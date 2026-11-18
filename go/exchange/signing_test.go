package exchange

import (
	"bytes"
	"encoding/hex"
	"encoding/json"
	"errors"
	"fmt"
	"strings"
	"sync"
	"testing"
	"time"

	"github.com/ethereum/go-ethereum/crypto"
	"github.com/riverbit-dex/riverbit-mm-sdk/go/binws"
)

const testVault = "0x0102030405060708090a0b0c0d0e0f1011121314"

type cDeposit struct {
	Type string `json:"type" msgpack:"type"`
}

// Connection-ID vectors for the minimal action {"type":"cDeposit"} with nonce
// 0x0102030405060708, without a vault and with the vault 0x0102..14.
func TestConnectionIDVectors(t *testing.T) {
	plain, err := newTestClient("https://example.invalid")
	if err != nil {
		t.Fatal(err)
	}
	got, err := plain.connectionID(cDeposit{"cDeposit"}, 0x0102030405060708)
	if err != nil {
		t.Fatal(err)
	}
	if want := "f95c8d88b9fef7a40a319f22b71053452e5da31126895f2c7c082b272f9b01ec"; hex.EncodeToString(got) != want {
		t.Fatalf("connectionId without vault = %x, want %s", got, want)
	}
	vault, err := newTestVaultClient("https://example.invalid", testVault)
	if err != nil {
		t.Fatal(err)
	}
	got, err = vault.connectionID(cDeposit{"cDeposit"}, 0x0102030405060708)
	if err != nil {
		t.Fatal(err)
	}
	// Tail = 0x01 || 20-byte vault 0x0102..14.
	if want := "de98d071b0f17c1852d73c01848a297bd79ac8fdb6638fcf38eec5081915be2f"; hex.EncodeToString(got) != want {
		t.Fatalf("connectionId with vault = %x, want %s", got, want)
	}
	// SetPrincipal only changes the queried account, never the signed tail.
	if err := plain.SetPrincipal(testVault); err != nil {
		t.Fatal(err)
	}
	again, _ := plain.connectionID(cDeposit{"cDeposit"}, 0x0102030405060708)
	if hex.EncodeToString(again) != "f95c8d88b9fef7a40a319f22b71053452e5da31126895f2c7c082b272f9b01ec" || plain.AccountAddress() != testVault {
		t.Fatalf("principal changed the signature tail or was not applied")
	}
	if err := vault.SetPrincipal(testVault); err == nil {
		t.Fatal("SetPrincipal on a vault client must fail")
	}
}

func TestPersonalSignRecoversSigner(t *testing.T) {
	client, err := newTestClient("https://example.invalid")
	if err != nil {
		t.Fatal(err)
	}
	login, err := binws.NewLogin("pre-api.riverbit.io", testNow, nil, client.Signer())
	if err != nil {
		t.Fatal(err)
	}
	if got, err := binws.RecoverLoginSigner("pre-api.riverbit.io", login); err != nil || got != client.SignerAddress() {
		t.Fatalf("recovered %s (%v), want %s", got, err, client.SignerAddress())
	}
}

// fakeExchange verifies signed /exchange request bodies for fake gateways.
type fakeExchange struct {
	t        *testing.T
	mu       sync.Mutex
	canceled map[string]bool // cloids reported canceled
}

// verify checks the wire shape and the vault-bound signature, then returns the node payload.
func (f *fakeExchange) verify(client *Client, body []byte) string {
	t := f.t
	keys, err := topLevelKeys(body)
	if err != nil || strings.Join(keys, ",") != "action,nonce,signature,vaultAddress" {
		t.Errorf("request keys = %v (%v): %s", keys, err, body)
	}
	var req struct {
		Action       json.RawMessage `json:"action"`
		Nonce        uint64          `json:"nonce"`
		Signature    Signature       `json:"signature"`
		VaultAddress string          `json:"vaultAddress"`
	}
	if err := json.Unmarshal(body, &req); err != nil || req.VaultAddress != testVault {
		t.Errorf("request = %s (%v)", body, err)
	}
	var head struct {
		Type string `json:"type"`
	}
	_ = json.Unmarshal(req.Action, &head)
	var action any
	switch head.Type {
	case "order":
		action = &Place{}
	case "cancel":
		action = &Cancel{}
	case "cancelByCloid":
		action = &CancelByCloid{}
	case "modify":
		action = &Modify{}
	case "batchModify":
		action = &BatchModify{}
	default:
		t.Errorf("unexpected action type %q", head.Type)
		return `{"status":"err","response":"bad"}`
	}
	if err := json.Unmarshal(req.Action, action); err != nil {
		t.Errorf("decode action: %v", err)
	}
	// The action must round-trip byte-identically: it is signed in this key order.
	if again, _ := json.Marshal(action); !bytes.Equal(again, req.Action) {
		t.Errorf("action JSON not in canonical struct order:\n got %s\nwant %s", req.Action, again)
	}
	digest, err := client.signingDigest(action, req.Nonce)
	if err != nil {
		t.Errorf("digest: %v", err)
	}
	raw, err := signatureBytes(req.Signature)
	if err != nil {
		t.Errorf("signature: %v", err)
	} else if pub, err := crypto.SigToPub(digest, raw); err != nil || strings.ToLower(crypto.PubkeyToAddress(*pub).Hex()) != client.SignerAddress() {
		t.Errorf("signature does not recover the signer with the vault tail")
	}
	switch a := action.(type) {
	case *Place:
		statuses := make([]string, len(a.Orders))
		for i := range a.Orders {
			statuses[i] = fmt.Sprintf(`{"resting":{"oid":%d}}`, 4242+i)
		}
		return `{"status":"ok","response":{"type":"order","data":{"statuses":[` + strings.Join(statuses, ",") + `]}}}`
	case *CancelByCloid:
		f.mu.Lock()
		for _, c := range a.Cancels {
			f.canceled[c.Cloid.String()] = true
		}
		f.mu.Unlock()
		return `{"status":"ok","response":{"type":"cancel","data":{"statuses":["success"]}}}`
	case *Cancel:
		return `{"status":"ok","response":{"type":"cancel","data":{"statuses":["success"]}}}`
	}
	return `{"status":"ok","response":{"type":"default"}}` // modify / batchModify acceptance
}

func topLevelKeys(body []byte) ([]string, error) {
	dec := json.NewDecoder(bytes.NewReader(body))
	if _, err := dec.Token(); err != nil {
		return nil, err
	}
	var keys []string
	for dec.More() {
		tok, err := dec.Token()
		if err != nil {
			return nil, err
		}
		keys = append(keys, tok.(string))
		var skip json.RawMessage
		if err := dec.Decode(&skip); err != nil {
			return nil, err
		}
	}
	return keys, nil
}

var testNow = time.UnixMilli(1760000000000)

func errorsAs(err error, target **ActionRejectedError) bool { return errors.As(err, target) }
