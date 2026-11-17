package exchange

import (
	"encoding/json"
	"fmt"
)

// The action types below are marshaled twice with the same field order: as
// JSON for the request body and as MessagePack for the signed connection ID.
// Field order is part of the signature. Do not replace these structs with
// maps, and do not reserialize an action after signing it.

// Limit is the limit-order parameters. TIF is "Alo" (maker only), "Gtc" or
// "Ioc".
type Limit struct {
	TIF string `json:"tif" msgpack:"tif"`
}

// OrderType selects the order kind. Only limit orders are supported.
type OrderType struct {
	Limit Limit `json:"limit" msgpack:"limit"`
}

// Order is one order leg: the element of Place.Orders and of a modify. Price
// and Size are decimal strings; they are signed as the exact strings given.
// Cloid, if set, is a 0x-prefixed 128-bit lowercase hex client order ID.
type Order struct {
	Asset      int       `json:"a" msgpack:"a"`
	Buy        bool      `json:"b" msgpack:"b"`
	Price      string    `json:"p" msgpack:"p"`
	Size       string    `json:"s" msgpack:"s"`
	ReduceOnly bool      `json:"r" msgpack:"r"`
	Type       OrderType `json:"t" msgpack:"t"`
	Cloid      string    `json:"c,omitempty" msgpack:"c,omitempty"`
}

// Place is the order action: it places one or more Orders.
type Place struct {
	Type     string  `json:"type" msgpack:"type"`
	Orders   []Order `json:"orders" msgpack:"orders"`
	Grouping string  `json:"grouping" msgpack:"grouping"`
}

// CancelItem cancels one order by asset and order ID.
type CancelItem struct {
	Asset int    `json:"a" msgpack:"a"`
	OID   uint64 `json:"o" msgpack:"o"`
}

// Cancel is the cancel-by-order-ID action.
type Cancel struct {
	Type    string       `json:"type" msgpack:"type"`
	Cancels []CancelItem `json:"cancels" msgpack:"cancels"`
}

// CancelByCloidItem cancels one order by asset and client order ID.
type CancelByCloidItem struct {
	Asset int   `json:"asset" msgpack:"asset"`
	Cloid Cloid `json:"cloid" msgpack:"cloid"`
}

// CancelByCloid is the cancel-by-client-order-ID action.
type CancelByCloid struct {
	Type    string              `json:"type" msgpack:"type"`
	Cancels []CancelByCloidItem `json:"cancels" msgpack:"cancels"`
	Fast    *bool               `json:"f,omitempty" msgpack:"f,omitempty"`
}

// Modify is the single-order modify action. Its acceptance does not return
// the new order ID; look the order up by Cloid. Prefer BatchModify.
type Modify struct {
	Type  string `json:"type" msgpack:"type"`
	OID   uint64 `json:"oid" msgpack:"oid"`
	Order Order  `json:"order" msgpack:"order"`
}

// ModifyItem replaces the order OID with Order.
type ModifyItem struct {
	OID   uint64 `json:"oid" msgpack:"oid"`
	Order Order  `json:"order" msgpack:"order"`
}

// BatchModify is the batch modify action. Its response carries the new order
// ID of every replacement.
type BatchModify struct {
	Type     string       `json:"type" msgpack:"type"`
	Modifies []ModifyItem `json:"modifies" msgpack:"modifies"`
}

// NewPlace returns the order action for orders, with Type and Grouping set.
func NewPlace(orders ...Order) *Place {
	return &Place{Type: "order", Orders: orders, Grouping: "na"}
}

// NewCancel returns the cancel action for items.
func NewCancel(items ...CancelItem) *Cancel {
	return &Cancel{Type: "cancel", Cancels: items}
}

// NewCancelByCloid returns the cancelByCloid action for items.
func NewCancelByCloid(items ...CancelByCloidItem) *CancelByCloid {
	return &CancelByCloid{Type: "cancelByCloid", Cancels: items}
}

// NewModify returns the modify action replacing oid with order.
func NewModify(oid uint64, order Order) *Modify {
	return &Modify{Type: "modify", OID: oid, Order: order}
}

// NewBatchModify returns the batchModify action for items.
func NewBatchModify(items ...ModifyItem) *BatchModify {
	return &BatchModify{Type: "batchModify", Modifies: items}
}

// LimitOrder returns a limit order leg. price and size are decimal strings;
// tif is "Alo", "Gtc" or "Ioc". Set Cloid or ReduceOnly on the result if needed.
func LimitOrder(asset int, buy bool, price, size, tif string) Order {
	return Order{Asset: asset, Buy: buy, Price: price, Size: size, Type: OrderType{Limit: Limit{TIF: tif}}}
}

// ParseAction decodes a JSON action of any supported type into its typed
// form (*Place, *Cancel, *CancelByCloid, *Modify or *BatchModify), selected by
// the "type" member. Unknown members are ignored, so re-marshal the result
// rather than sending the input bytes if you need the canonical key order.
func ParseAction(raw []byte) (any, error) {
	var head struct {
		Type string `json:"type"`
	}
	if err := json.Unmarshal(raw, &head); err != nil {
		return nil, err
	}
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
		return nil, fmt.Errorf("unsupported action type %q", head.Type)
	}
	if err := json.Unmarshal(raw, action); err != nil {
		return nil, err
	}
	return action, nil
}
