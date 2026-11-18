package exchange

import (
	"fmt"
	"net/http"
)

// GatewayCredentialHeader is the HTTP header that carries the optional gateway
// credential.
const GatewayCredentialHeader = "X-RiverBit-MM-Token"

// gatewayToken holds the optional gateway credential behind a pointer, so that
// printing a Client or InfoClient (even with %+v or %#v) shows an address, not
// the credential. Its own String methods redact it too.
type gatewayToken struct{ value string }

func (*gatewayToken) String() string   { return "[gateway credential]" }
func (*gatewayToken) GoString() string { return "[gateway credential]" }

// apply sets the header on req; a nil or empty token sets nothing.
func (t *gatewayToken) apply(req *http.Request) {
	if t != nil && t.value != "" {
		req.Header.Set(GatewayCredentialHeader, t.value)
	}
}

// newGatewayToken validates credential. An empty one clears the setting. A
// credential with a character that is invalid in an HTTP header value (a control
// character, CR and LF included, or a non-ASCII byte) is refused; the error
// never contains the value.
func newGatewayToken(credential string) (*gatewayToken, error) {
	if credential == "" {
		return nil, nil
	}
	for i := 0; i < len(credential); i++ {
		if b := credential[i]; b < 0x20 || b >= 0x7f {
			return nil, fmt.Errorf("gateway credential contains a character that is invalid in an HTTP header value (byte %d)", i)
		}
	}
	return &gatewayToken{value: credential}, nil
}

// SetGatewayCredential sets the optional gateway credential RiverBit may issue to
// a market maker to lift the per-IP REST limits. When set, every REST request of
// this client (/info and /exchange) carries the header X-RiverBit-MM-Token. An
// empty string clears it.
//
// The credential is only sent to the configured URL (redirects are never
// followed), never over bin-ws, and never logged, journaled or put in an error
// message. It does not change signing, nonces or the taker delay; an invalid
// credential is silently treated by the server as no credential. A credential
// with a character that is invalid in an HTTP header value is refused here.
func (c *InfoClient) SetGatewayCredential(credential string) error {
	t, err := newGatewayToken(credential)
	if err != nil {
		return err
	}
	c.token = t
	return nil
}

// SetGatewayCredential sets the optional gateway credential for REST /info and
// /exchange; see InfoClient.SetGatewayCredential. InfoClient and LoadChainID use
// it too.
func (c *Client) SetGatewayCredential(credential string) error {
	t, err := newGatewayToken(credential)
	if err != nil {
		return err
	}
	c.token = t
	return nil
}
