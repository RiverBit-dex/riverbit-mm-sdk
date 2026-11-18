package exchange

import (
	"encoding/json"
	"fmt"
)

// LegStatus is the result of one leg of an action: one order of an order
// action, one target of a cancel, one replacement of a batch modify. Legs are
// reported in request order.
type LegStatus struct {
	// Kind is "success" (cancel), "resting", "filled", "error",
	// "waitingForTrigger", "waitingForFill" or another value the server
	// sends; the raw JSON is in Raw.
	Kind string
	// OID is the order ID of a resting or filled leg; for a replacement it is
	// the new order ID.
	OID uint64
	// TotalSz and AvgPx are the decimal strings of a filled leg. A filled leg
	// can still leave a resting remainder: query orderStatus for live state.
	TotalSz, AvgPx string
	// Error is the rejection text of an error leg.
	Error string
	// Raw is the leg exactly as received.
	Raw json.RawMessage
}

// Receipt is the parsed reply to a submitted action. Inspect every leg: a
// receipt with Status "ok" can contain rejected legs, and partial success is
// not rolled back. A reply with Status "err" may still carry committed legs.
type Receipt struct {
	Status string // "ok" or "err"
	// Type is the response type: "order", "cancel", "default", ...
	Type string
	// Legs are the per-leg results; empty for a whole-action rejection and for
	// an accepted modify ({"type":"default"}).
	Legs []LegStatus
	// Raw is the reply exactly as received.
	Raw json.RawMessage
}

// Rejected returns the legs that carry an error.
func (r *Receipt) Rejected() []LegStatus {
	var out []LegStatus
	for _, l := range r.Legs {
		if l.Kind == "error" {
			out = append(out, l)
		}
	}
	return out
}

// ParseReceipt parses an /exchange reply.
func ParseReceipt(raw []byte) (*Receipt, error) {
	var reply struct {
		Status          string          `json:"status"`
		Response        json.RawMessage `json:"response"`
		CommittedResult json.RawMessage `json:"committedResult"`
	}
	if err := json.Unmarshal(raw, &reply); err != nil {
		return nil, fmt.Errorf("invalid exchange reply: %w", err)
	}
	r := &Receipt{Status: reply.Status, Raw: append(json.RawMessage(nil), raw...)}
	body := reply.Response
	if reply.Status == "err" {
		body = reply.CommittedResult
	}
	var response struct {
		Type string `json:"type"`
		Data struct {
			Statuses []json.RawMessage `json:"statuses"`
		} `json:"data"`
	}
	if len(body) == 0 || json.Unmarshal(body, &response) != nil {
		return r, nil // a plain error text or an empty body: no legs
	}
	r.Type = response.Type
	for _, s := range response.Data.Statuses {
		r.Legs = append(r.Legs, parseLeg(s))
	}
	return r, nil
}

func parseLeg(raw json.RawMessage) LegStatus {
	leg := LegStatus{Raw: append(json.RawMessage(nil), raw...)}
	var word string
	if json.Unmarshal(raw, &word) == nil {
		leg.Kind = word
		return leg
	}
	var obj map[string]json.RawMessage
	if json.Unmarshal(raw, &obj) != nil || len(obj) != 1 {
		leg.Kind = "unknown"
		return leg
	}
	for kind, value := range obj {
		leg.Kind = kind
		switch kind {
		case "error":
			_ = json.Unmarshal(value, &leg.Error)
		case "resting", "filled":
			var fields struct {
				OID     uint64 `json:"oid"`
				TotalSz string `json:"totalSz"`
				AvgPx   string `json:"avgPx"`
			}
			_ = json.Unmarshal(value, &fields)
			leg.OID, leg.TotalSz, leg.AvgPx = fields.OID, fields.TotalSz, fields.AvgPx
		}
	}
	return leg
}
