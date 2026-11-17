package exchange

import (
	"bytes"
	"encoding/hex"
	"encoding/json"
	"fmt"
	"os"
	"strings"
	"testing"

	"github.com/riverbit-dex/riverbit-mm-sdk/go/binws"
)

// vectorsPath is the shared golden file; see testdata/README.md.
const vectorsPath = "../../testdata/vectors.json"

type vectorFile struct {
	TestKey struct {
		PrivateKey string `json:"private_key"`
		Address    string `json:"address"`
	} `json:"test_key"`
	Login []struct {
		Name         string `json:"name"`
		Host         string `json:"host"`
		TimestampMs  uint64 `json:"timestamp_ms"`
		NonceHex     string `json:"nonce_hex"`
		Message      string `json:"message"`
		SignatureHex string `json:"signature_hex"`
	} `json:"login"`
	Actions []struct {
		Name         string  `json:"name"`
		ChainID      uint64  `json:"chain_id"`
		Source       string  `json:"source"`
		Nonce        uint64  `json:"nonce"`
		VaultAddress *string `json:"vault_address"`
		ActionJSON   string  `json:"action_json"`
		MsgpackHex   string  `json:"msgpack_hex"`
		ConnectionID string  `json:"connection_id"`
		Digest       string  `json:"digest"`
		Signature    struct {
			R string `json:"r"`
			S string `json:"s"`
			V byte   `json:"v"`
		} `json:"signature"`
	} `json:"actions"`
}

func loadVectors(t *testing.T) vectorFile {
	t.Helper()
	raw, err := os.ReadFile(vectorsPath)
	if err != nil {
		t.Fatal(err)
	}
	var v vectorFile
	if err := json.Unmarshal(raw, &v); err != nil {
		t.Fatal(err)
	}
	if len(v.Login) < 2 || len(v.Actions) < 13 {
		t.Fatalf("vectors.json is incomplete: %d login, %d action vectors", len(v.Login), len(v.Actions))
	}
	return v
}

// pad64 left-pads a 0x-prefixed hexadecimal scalar to 64 digits, the form used
// in the vectors file (the library prints it without leading zeroes).
func pad64(s string) string {
	s = strings.TrimPrefix(s, "0x")
	return "0x" + strings.Repeat("0", 64-len(s)) + s
}

func TestLoginVectors(t *testing.T) {
	v := loadVectors(t)
	signer, err := NewSigner(v.TestKey.PrivateKey)
	if err != nil {
		t.Fatal(err)
	}
	if !strings.EqualFold(signer.Address(), v.TestKey.Address) {
		t.Fatalf("test key address %s, want %s", signer.Address(), v.TestKey.Address)
	}
	for _, tc := range v.Login {
		t.Run(tc.Name, func(t *testing.T) {
			nonce, err := hex.DecodeString(tc.NonceHex)
			if err != nil {
				t.Fatal(err)
			}
			message := binws.LoginMessage(tc.Host, tc.TimestampMs, nonce)
			if message != tc.Message {
				t.Fatalf("message %q, want %q", message, tc.Message)
			}
			sig, err := signer.PersonalSign([]byte(message))
			if err != nil {
				t.Fatal(err)
			}
			if got := hex.EncodeToString(sig); got != tc.SignatureHex {
				t.Fatalf("signature %s, want %s", got, tc.SignatureHex)
			}
			login := binws.Login{TimestampMs: tc.TimestampMs, Nonce: nonce, Signature: sig}
			if got, err := binws.RecoverLoginSigner(tc.Host, login); err != nil || got != signer.Address() {
				t.Fatalf("recovered %s (%v), want %s", got, err, signer.Address())
			}
		})
	}
}

func TestActionVectors(t *testing.T) {
	v := loadVectors(t)
	signer, err := NewSigner(v.TestKey.PrivateKey)
	if err != nil {
		t.Fatal(err)
	}
	for _, tc := range v.Actions {
		t.Run(tc.Name, func(t *testing.T) {
			if tc.Source != SigningSource {
				t.Fatalf("vector source %q, library signs with %q", tc.Source, SigningSource)
			}
			var client *Client
			if tc.VaultAddress == nil {
				client, err = New("https://example.invalid", signer)
			} else {
				client, err = NewForVault("https://example.invalid", signer, *tc.VaultAddress)
			}
			if err != nil {
				t.Fatal(err)
			}
			client.SignChainID = tc.ChainID
			action, err := ParseAction([]byte(tc.ActionJSON))
			if err != nil {
				t.Fatal(err)
			}
			// The signed bytes and the sent bytes are the same bytes.
			if sent, err := json.Marshal(action); err != nil || string(sent) != tc.ActionJSON {
				t.Fatalf("action JSON\n got %s\nwant %s (%v)", sent, tc.ActionJSON, err)
			}
			packed, err := msgpackAction(action)
			if err != nil {
				t.Fatal(err)
			}
			if got := hex.EncodeToString(packed); got != tc.MsgpackHex {
				t.Fatalf("msgpack %s, want %s", got, tc.MsgpackHex)
			}
			connection, err := client.connectionID(action, tc.Nonce)
			if err != nil {
				t.Fatal(err)
			}
			if got := "0x" + hex.EncodeToString(connection); got != tc.ConnectionID {
				t.Fatalf("connectionId %s, want %s", got, tc.ConnectionID)
			}
			digest, err := client.signingDigest(action, tc.Nonce)
			if err != nil {
				t.Fatal(err)
			}
			if got := "0x" + hex.EncodeToString(digest); got != tc.Digest {
				t.Fatalf("digest %s, want %s", got, tc.Digest)
			}
			sig, err := client.Sign(action, tc.Nonce)
			if err != nil {
				t.Fatal(err)
			}
			if pad64(sig.R) != tc.Signature.R || pad64(sig.S) != tc.Signature.S || sig.V != tc.Signature.V {
				t.Fatalf("signature %+v, want %+v", sig, tc.Signature)
			}
			// The complete request carries the same action bytes and, for a vault, vaultAddress.
			request, err := client.SignedRequest(action, tc.Nonce)
			if err != nil {
				t.Fatal(err)
			}
			wantPrefix := fmt.Sprintf(`{"action":%s,"nonce":%d,"signature":`, tc.ActionJSON, tc.Nonce)
			if !bytes.HasPrefix(request, []byte(wantPrefix)) {
				t.Fatalf("request does not start with the signed action:\n%s", request)
			}
			if hasVault := bytes.Contains(request, []byte(`"vaultAddress"`)); hasVault != (tc.VaultAddress != nil) {
				t.Fatalf("vaultAddress presence = %t in %s", hasVault, request)
			}
		})
	}
}
