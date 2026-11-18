package exchange

import (
	"encoding/json"
	"fmt"
	"strings"
	"sync"
	"time"
)

// A throwaway key (never a real wallet): 0x00..01.
const testKeyHex = "0000000000000000000000000000000000000000000000000000000000000001"

func newTestClient(url string) (*Client, error) {
	signer, err := NewSigner(testKeyHex)
	if err != nil {
		return nil, err
	}
	return New(url, signer)
}

func newTestVaultClient(url, vault string) (*Client, error) {
	signer, err := NewSigner(strings.Repeat("0", 63) + "1")
	if err != nil {
		return nil, err
	}
	return NewForVault(url, signer, vault)
}

// orderLegs returns the order legs of a place, modify or batchModify action, in
// request order.
func orderLegs(action any) []Order {
	switch a := action.(type) {
	case *Place:
		return a.Orders
	case *Modify:
		return []Order{a.Order}
	case *BatchModify:
		var out []Order
		for _, m := range a.Modifies {
			out = append(out, m.Order)
		}
		return out
	}
	return nil
}

// orderStatusBody is an orderStatus answer for the order o as the node would
// report it: created now, with the submitted side, price and size.
func orderStatusBody(height int, status string, oid uint64, o Order) string {
	side := "A"
	if o.Buy {
		side = "B"
	}
	return fmt.Sprintf(`{"height":%d,"status":"order","order":{"status":%q,"order":{"coin":"RBT","oid":%d,"side":%q,"limitPx":%q,"sz":%q,"timestamp":%d,"cloid":%q}}}`,
		height, status, oid, side, o.Price, o.Size, time.Now().UnixMilli(), o.Cloid)
}

// cloidBook remembers the order legs a fake gateway received, by cloid.
type cloidBook struct {
	mu     sync.Mutex
	orders map[string]Order
}

func (b *cloidBook) record(body []byte) {
	var req struct {
		Action json.RawMessage `json:"action"`
	}
	if json.Unmarshal(body, &req) != nil {
		return
	}
	action, err := ParseAction(req.Action)
	if err != nil {
		return
	}
	b.mu.Lock()
	defer b.mu.Unlock()
	if b.orders == nil {
		b.orders = map[string]Order{}
	}
	for _, o := range orderLegs(action) {
		b.orders[o.Cloid] = o
	}
}

func (b *cloidBook) get(cloid string) (Order, bool) {
	b.mu.Lock()
	defer b.mu.Unlock()
	o, ok := b.orders[cloid]
	return o, ok
}
