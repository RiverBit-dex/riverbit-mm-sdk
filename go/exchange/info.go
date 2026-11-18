package exchange

import (
	"bytes"
	"context"
	"encoding/json"
	"fmt"
	"io"
	"net/http"
	"strconv"
	"strings"
	"time"
)

// InfoClient sends unsigned read-only queries to POST <base>/info. It needs no
// key. 429 replies are retried with the server's Retry-After (or a capped
// exponential backoff) until the context ends.
type InfoClient struct {
	// URL is the REST base URL, for example "https://pre-api.riverbit.io".
	URL string
	// HTTP is the HTTP client; nil uses a client with a 15 second timeout.
	HTTP *http.Client
}

// NewInfoClient returns an InfoClient for the REST base URL.
func NewInfoClient(baseURL string) *InfoClient {
	return &InfoClient{URL: strings.TrimRight(baseURL, "/")}
}

// Query POSTs body as JSON to /info and decodes the JSON answer into out.
// body is usually a map or struct with a "type" member, for example
// {"type":"openOrders","user":"0x..."}.
func (c *InfoClient) Query(ctx context.Context, body, out any) error {
	return retryRateLimited(ctx, func() error { return postJSON(ctx, c.http(), c.URL, "/info", body, out) })
}

// Check verifies that /info is reachable by sending a meta query.
func (c *InfoClient) Check(ctx context.Context) error {
	var out json.RawMessage
	return c.Query(ctx, map[string]string{"type": "meta"}, &out)
}

func (c *InfoClient) http() *http.Client {
	if c.HTTP != nil {
		return c.HTTP
	}
	return &http.Client{Timeout: 15 * time.Second}
}

// ChainIdentity is the answer of the chainIdentity query.
type ChainIdentity struct {
	NextAgentChainID uint64 `json:"nextAgentChainId"`
	AgentChainID     uint64 `json:"agentChainId"`
}

// SignChainID returns the chain ID to sign trading actions with:
// nextAgentChainId, or agentChainId on older gateways. It is zero if the
// gateway reported neither.
func (i ChainIdentity) SignChainID() uint64 {
	if i.NextAgentChainID != 0 {
		return i.NextAgentChainID
	}
	return i.AgentChainID
}

// ChainIdentity queries the chain identity used for signing.
func (c *InfoClient) ChainIdentity(ctx context.Context) (ChainIdentity, error) {
	var id ChainIdentity
	err := c.Query(ctx, map[string]any{"type": "chainIdentity"}, &id)
	return id, err
}

// Asset is one entry of the metaAndAssetCtxs universe.
type Asset struct {
	Index       int    // position in the universe: the order field "a"
	Name        string // coin name
	SzDecimals  int    // size precision
	MaxLeverage int
}

// Assets queries metaAndAssetCtxs and returns the universe in index order.
// Query it at every startup instead of hard-coding asset indexes.
func (c *InfoClient) Assets(ctx context.Context) ([]Asset, error) {
	var snapshot []json.RawMessage
	if err := c.Query(ctx, map[string]any{"type": "metaAndAssetCtxs"}, &snapshot); err != nil {
		return nil, fmt.Errorf("read market asset info: %w", err)
	}
	if len(snapshot) == 0 {
		return nil, fmt.Errorf("metaAndAssetCtxs returned no market metadata")
	}
	var meta struct {
		Universe []struct {
			Name        string `json:"name"`
			SzDecimals  int    `json:"szDecimals"`
			MaxLeverage int    `json:"maxLeverage"`
		} `json:"universe"`
	}
	if err := json.Unmarshal(snapshot[0], &meta); err != nil {
		return nil, fmt.Errorf("decode market metadata: %w", err)
	}
	out := make([]Asset, len(meta.Universe))
	for i, a := range meta.Universe {
		out[i] = Asset{Index: i, Name: a.Name, SzDecimals: a.SzDecimals, MaxLeverage: a.MaxLeverage}
	}
	return out, nil
}

// Asset looks up one coin in metaAndAssetCtxs.
func (c *InfoClient) Asset(ctx context.Context, coin string) (Asset, error) {
	assets, err := c.Assets(ctx)
	if err != nil {
		return Asset{}, err
	}
	for _, a := range assets {
		if a.Name == coin {
			return a, nil
		}
	}
	return Asset{}, fmt.Errorf("market %q is absent from metaAndAssetCtxs", coin)
}

// UserRole is the answer of the userRole query. Role is "agent" (Data.User is
// the master account), "user", "subAccount", "vault" or "missing". The query
// is expensive (rate-limit weight 60): call it at startup only.
type UserRole struct {
	Role string `json:"role"`
	Data struct {
		User   string `json:"user"`
		Master string `json:"master"`
	} `json:"data"`
}

// UserRole resolves an address to its role and master account.
func (c *InfoClient) UserRole(ctx context.Context, user string) (UserRole, error) {
	var role UserRole
	err := c.Query(ctx, map[string]any{"type": "userRole", "user": user}, &role)
	return role, err
}

// OpenOrder is one entry of an openOrders answer (and, with Cloid, of
// frontendOpenOrders). Price and Size are decimal strings.
type OpenOrder struct {
	Coin  string `json:"coin"`
	OID   uint64 `json:"oid"`
	Side  string `json:"side"` // "B" buy, "A" sell
	Price string `json:"limitPx"`
	Size  string `json:"sz"` // remaining size
	Cloid string `json:"cloid"`
}

// OpenOrders lists the open orders of user (a master account or vault).
func (c *InfoClient) OpenOrders(ctx context.Context, user string) ([]OpenOrder, error) {
	var orders []OpenOrder
	err := c.Query(ctx, map[string]any{"type": "openOrders", "user": user}, &orders)
	return orders, err
}

// postJSON POSTs body as JSON to base+path and decodes a 200 answer into out.
// Any other status is an error; 429 is a *RateLimitError. For /exchange a
// non-200 answer means the outcome is unknown, not rejected.
func postJSON(ctx context.Context, client *http.Client, base, path string, body, out any) error {
	data, err := json.Marshal(body)
	if err != nil {
		return err
	}
	return postRaw(ctx, client, base, path, data, out)
}

// postRaw POSTs data unchanged to base+path.
func postRaw(ctx context.Context, client *http.Client, base, path string, data []byte, out any) error {
	req, err := http.NewRequestWithContext(ctx, "POST", base+path, bytes.NewReader(data))
	if err != nil {
		return err
	}
	req.Header.Set("Content-Type", "application/json")
	resp, err := client.Do(req)
	if err != nil {
		return err
	}
	defer resp.Body.Close()
	raw, err := io.ReadAll(io.LimitReader(resp.Body, 4<<20))
	if err != nil {
		return err
	}
	if resp.StatusCode == http.StatusTooManyRequests {
		limit := &RateLimitError{Path: path, Body: truncate(raw, 500)}
		limit.RetryAfter = parseRetryAfter(resp.Header.Get("Retry-After"), time.Now())
		return limit
	}
	if resp.StatusCode != 200 {
		return fmt.Errorf("%s HTTP %d: %.500s", path, resp.StatusCode, raw)
	}
	if err = json.Unmarshal(raw, out); err != nil {
		return fmt.Errorf("%s invalid JSON: %w", path, err)
	}
	return nil
}

// parseRetryAfter reads a Retry-After value: whole seconds or an HTTP date.
// It returns zero when the value is absent, malformed or not in the future.
func parseRetryAfter(value string, now time.Time) time.Duration {
	value = strings.TrimSpace(value)
	if secs, err := strconv.Atoi(value); err == nil {
		if secs < 0 {
			return 0
		}
		return time.Duration(secs) * time.Second
	}
	if at, err := http.ParseTime(value); err == nil {
		if d := at.Sub(now); d > 0 {
			return d
		}
	}
	return 0
}

func truncate(b []byte, n int) string {
	if len(b) > n {
		b = b[:n]
	}
	return string(b)
}
