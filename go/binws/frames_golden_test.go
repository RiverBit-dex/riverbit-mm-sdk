package binws

import (
	"bytes"
	"encoding/hex"
	"encoding/json"
	"fmt"
	"os"
	"reflect"
	"testing"
)

// This file checks testdata/frames.json, the shared golden bin-ws frames.
// Run `UPDATE_FRAMES=1 go test ./binws -run TestFramesGolden` from go/ to
// regenerate the file from the sample messages below.

const framesPath = "../../testdata/frames.json"

// obj is a JSON object that keeps insertion order.
type obj []kv
type kv struct {
	K string
	V any
}

func (o obj) MarshalJSON() ([]byte, error) {
	var b bytes.Buffer
	b.WriteByte('{')
	for i, e := range o {
		if i > 0 {
			b.WriteByte(',')
		}
		k, _ := json.Marshal(e.K)
		v, err := json.Marshal(e.V)
		if err != nil {
			return nil, err
		}
		b.Write(k)
		b.WriteByte(':')
		b.Write(v)
	}
	b.WriteByte('}')
	return b.Bytes(), nil
}

func hx(b []byte) string { return hex.EncodeToString(b) }

func optU64(p *uint64) any {
	if p == nil {
		return nil
	}
	return *p
}
func optU32(p *uint32) any {
	if p == nil {
		return nil
	}
	return *p
}

// setOpt appends the field only when the optional value is present.
func setOpt(o obj, name string, v any) obj {
	if v == nil {
		return o
	}
	return append(o, kv{name, v})
}

func fixedList(vals []uint64, decimals uint32) []string {
	out := make([]string, len(vals))
	for i, v := range vals {
		out[i] = FormatFixed(v, decimals)
	}
	return out
}

func describeBook(b L2BookFrame) obj {
	return obj{{"coin", b.Coin}, {"time", b.Time}, {"px_decimals", b.PxDecimals}, {"sz_decimals", b.SzDecimals},
		{"bid_px", orEmpty(b.BidPx)}, {"bid_sz", orEmpty(b.BidSz)}, {"bid_n", orEmpty32(b.BidN)},
		{"ask_px", orEmpty(b.AskPx)}, {"ask_sz", orEmpty(b.AskSz)}, {"ask_n", orEmpty32(b.AskN)}}
}

func orEmpty(v []uint64) []uint64 {
	if v == nil {
		return []uint64{}
	}
	return v
}
func orEmpty32(v []uint32) []uint32 {
	if v == nil {
		return []uint32{}
	}
	return v
}

// describeClient renders a decoded client frame with protobuf field names.
func describeClient(f ClientFrame) (obj, error) {
	o := obj{{"id", f.ID}}
	switch f.Body {
	case ClientAction:
		return append(o, kv{"body", "action_json"}, kv{"json", string(f.Data)}), nil
	case ClientInfo:
		return append(o, kv{"body", "info_json"}, kv{"json", string(f.Data)}), nil
	case ClientSubscribe:
		return append(o, kv{"body", "subscribe_json"}, kv{"json", string(f.Data)}), nil
	case ClientUnsubscribe:
		return append(o, kv{"body", "unsubscribe_json"}, kv{"json", string(f.Data)}), nil
	case ClientPing:
		return append(o, kv{"body", "ping"}), nil
	case ClientLogin:
		l, err := UnmarshalLogin(f.Data)
		if err != nil {
			return nil, err
		}
		return append(o, kv{"body", "login"}, kv{"message", obj{{"timestamp_ms", l.TimestampMs}, {"nonce_hex", hx(l.Nonce)}, {"signature_hex", hx(l.Signature)}}}), nil
	}
	return nil, fmt.Errorf("unknown client body %d", f.Body)
}

// describeServer renders a decoded server frame with protobuf field names.
// Fixed-point values are shown as integers; the "decimal" member shows the
// same prices and sizes as decimal strings (value = integer / 10^decimals).
func describeServer(f ServerFrame) (obj, error) {
	o := obj{{"id", f.ID}}
	switch f.Kind {
	case KindJSON:
		return append(o, kv{"body", "json"}, kv{"json", string(f.Data)}), nil
	case KindLogin:
		r, err := UnmarshalLoginResult(f.Data)
		if err != nil {
			return nil, err
		}
		return append(o, kv{"body", "login"}, kv{"message", obj{{"ok", r.OK}, {"user_hex", hx(r.User)}, {"signer_hex", hx(r.Signer)}, {"error", r.Error}}}), nil
	case KindL2Book:
		b, err := UnmarshalL2Book(f.Data)
		if err != nil {
			return nil, err
		}
		return append(o, kv{"body", "l2_book"}, kv{"message", describeBook(b)},
			kv{"decimal", obj{{"bid_px", fixedList(b.BidPx, b.PxDecimals)}, {"bid_sz", fixedList(b.BidSz, b.SzDecimals)}, {"ask_px", fixedList(b.AskPx, b.PxDecimals)}, {"ask_sz", fixedList(b.AskSz, b.SzDecimals)}}}), nil
	case KindL2Delta:
		d, err := UnmarshalL2Delta(f.Data)
		if err != nil {
			return nil, err
		}
		if d.Snapshot != nil {
			b := *d.Snapshot
			return append(o, kv{"body", "l2_delta.snapshot"}, kv{"message", describeBook(b)},
				kv{"decimal", obj{{"bid_px", fixedList(b.BidPx, b.PxDecimals)}, {"bid_sz", fixedList(b.BidSz, b.SzDecimals)}, {"ask_px", fixedList(b.AskPx, b.PxDecimals)}, {"ask_sz", fixedList(b.AskSz, b.SzDecimals)}}}), nil
		}
		u := *d.Update
		m := obj{{"coin", u.Coin}, {"time", u.Time}, {"px_decimals", u.PxDecimals}, {"sz_decimals", u.SzDecimals},
			{"bid_px", orEmpty(u.BidPx)}, {"bid_sz", orEmpty(u.BidSz)}, {"ask_px", orEmpty(u.AskPx)}, {"ask_sz", orEmpty(u.AskSz)},
			{"bid_removed", orEmpty32(u.BidRemoved)}, {"ask_removed", orEmpty32(u.AskRemoved)}}
		m = setOpt(m, "spread", optU64(u.Spread))
		dec := obj{{"bid_px", fixedList(u.BidPx, u.PxDecimals)}, {"bid_sz", fixedList(u.BidSz, u.SzDecimals)}, {"ask_px", fixedList(u.AskPx, u.PxDecimals)}, {"ask_sz", fixedList(u.AskSz, u.SzDecimals)}}
		if u.Spread != nil {
			dec = append(dec, kv{"spread", FormatFixed(*u.Spread, u.PxDecimals)})
		}
		return append(o, kv{"body", "l2_delta.update"}, kv{"message", m}, kv{"decimal", dec}), nil
	case KindBBO:
		b, err := UnmarshalBBO(f.Data)
		if err != nil {
			return nil, err
		}
		m := obj{{"coin", b.Coin}, {"time", b.Time}, {"px_decimals", b.PxDecimals}, {"sz_decimals", b.SzDecimals}}
		m = setOpt(m, "bid_px", optU64(b.BidPx))
		m = setOpt(m, "bid_sz", optU64(b.BidSz))
		m = setOpt(m, "bid_n", optU32(b.BidN))
		m = setOpt(m, "ask_px", optU64(b.AskPx))
		m = setOpt(m, "ask_sz", optU64(b.AskSz))
		m = setOpt(m, "ask_n", optU32(b.AskN))
		var dec obj
		if b.BidPx != nil && b.BidSz != nil {
			dec = append(dec, kv{"bid_px", FormatFixed(*b.BidPx, b.PxDecimals)}, kv{"bid_sz", FormatFixed(*b.BidSz, b.SzDecimals)})
		}
		if b.AskPx != nil && b.AskSz != nil {
			dec = append(dec, kv{"ask_px", FormatFixed(*b.AskPx, b.PxDecimals)}, kv{"ask_sz", FormatFixed(*b.AskSz, b.SzDecimals)})
		}
		if dec == nil {
			dec = obj{}
		}
		return append(o, kv{"body", "bbo"}, kv{"message", m}, kv{"decimal", dec}), nil
	case KindCandle:
		c, err := UnmarshalCandle(f.Data)
		if err != nil {
			return nil, err
		}
		return append(o, kv{"body", "candle"}, kv{"message", obj{{"coin", c.Coin}, {"interval", c.Interval}, {"open_time", c.OpenTime}, {"close_time", c.CloseTime},
			{"px_decimals", c.PxDecimals}, {"sz_decimals", c.SzDecimals}, {"open", c.Open}, {"close", c.Close}, {"high", c.High}, {"low", c.Low},
			{"volume", c.Volume}, {"trade_count", c.TradeCount}}},
			kv{"decimal", obj{{"open", FormatFixed(c.Open, c.PxDecimals)}, {"close", FormatFixed(c.Close, c.PxDecimals)}, {"high", FormatFixed(c.High, c.PxDecimals)},
				{"low", FormatFixed(c.Low, c.PxDecimals)}, {"volume", FormatFixed(c.Volume, c.SzDecimals)}}}), nil
	case KindTrades:
		t, err := UnmarshalTrades(f.Data)
		if err != nil {
			return nil, err
		}
		var trades, dec []obj
		for _, tr := range t.Trades {
			trades = append(trades, obj{{"buy", tr.Buy}, {"px", tr.Px}, {"sz", tr.Sz}, {"time", tr.Time}, {"tid", tr.TID},
				{"hash_hex", hx(tr.Hash)}, {"buyer_hex", hx(tr.Buyer)}, {"seller_hex", hx(tr.Seller)}})
			dec = append(dec, obj{{"px", FormatFixed(tr.Px, t.PxDecimals)}, {"sz", FormatFixed(tr.Sz, t.SzDecimals)}})
		}
		return append(o, kv{"body", "trades"}, kv{"message", obj{{"coin", t.Coin}, {"px_decimals", t.PxDecimals}, {"sz_decimals", t.SzDecimals}, {"trades", trades}}},
			kv{"decimal", obj{{"trades", dec}}}), nil
	}
	return nil, fmt.Errorf("unknown server kind %d", f.Kind)
}

type goldenFrame struct {
	Name      string
	Direction string // "client" or "server"
	Hex       string
	Frame     any
	// BookAfter, for an l2 update, is the book after applying it to AppliesTo.
	AppliesTo string
	BookAfter any
}

func bookObj(b *Book) obj {
	side := func(ls []Level) []obj {
		out := []obj{}
		for _, l := range ls {
			out = append(out, obj{{"px", l.PxText}, {"sz", l.SzText}})
		}
		return out
	}
	return obj{{"bids", side(b.Bids)}, {"asks", side(b.Asks)}}
}

func sampleFrames(t *testing.T) []goldenFrame {
	t.Helper()
	var out []goldenFrame
	client := func(name string, f ClientFrame) {
		raw := f.Marshal()
		dec, err := UnmarshalClientFrame(raw)
		if err != nil {
			t.Fatal(err)
		}
		d, err := describeClient(dec)
		if err != nil {
			t.Fatal(err)
		}
		out = append(out, goldenFrame{Name: name, Direction: "client", Hex: hx(raw), Frame: d})
	}
	server := func(name string, f ServerFrame) {
		raw := f.Marshal()
		dec, err := UnmarshalServerFrame(raw)
		if err != nil {
			t.Fatal(err)
		}
		d, err := describeServer(dec)
		if err != nil {
			t.Fatal(err)
		}
		out = append(out, goldenFrame{Name: name, Direction: "server", Hex: hx(raw), Frame: d})
	}
	nonce, _ := hex.DecodeString("000102030405060708090a0b0c0d0e0f")
	sig, _ := hex.DecodeString("4752b41eb2baa20a3e169891002152aa388130e129b3c8dbd4f957e03a1cddef7f1e12c690cbc3a8ec00f287739ea28e2a8d10ba6073353074735cc6fe56c23f1b")
	signer, _ := hex.DecodeString("1a642f0e3c3af545e7acbd38b07251b3990914f1")
	master, _ := hex.DecodeString("2222222222222222222222222222222222222222")

	client("client-login", ClientFrame{Body: ClientLogin, Data: Login{TimestampMs: 1792168694973, Nonce: nonce, Signature: sig}.Marshal()})
	client("client-ping", ClientFrame{Body: ClientPing})
	client("client-subscribe-l2", ClientFrame{Body: ClientSubscribe, Data: []byte(`{"type":"l2","c":"BTC"}`)})
	client("client-info-open-orders", ClientFrame{ID: 7, Body: ClientInfo, Data: []byte(`{"type":"openOrders","user":"0x2222222222222222222222222222222222222222"}`)})
	server("server-login-ok", ServerFrame{Kind: KindLogin, Data: LoginResult{OK: true, User: master, Signer: signer}.Marshal()})
	server("server-login-rejected", ServerFrame{Kind: KindLogin, Data: LoginResult{Error: "bad signature"}.Marshal()})

	snapshot := L2BookFrame{Coin: "BTC", Time: 1792168695000, PxDecimals: 1, SzDecimals: 5,
		BidPx: []uint64{800005, 800000, 799990}, BidSz: []uint64{150000, 200000, 50000}, BidN: []uint32{1, 2, 1},
		AskPx: []uint64{800010, 800020}, AskSz: []uint64{100000, 300000}, AskN: []uint32{1, 3}}
	server("server-l2-snapshot", ServerFrame{Kind: KindL2Delta, Data: L2DeltaFrame{Snapshot: &snapshot}.Marshal()})
	spread := uint64(5)
	update := L2DeltaUpdate{Coin: "BTC", Time: 1792168695100, PxDecimals: 1, SzDecimals: 5,
		BidPx: []uint64{800000, 800003}, BidSz: []uint64{250000, 75000}, // resize 80000.0, insert 80000.3
		AskPx: []uint64{800015}, AskSz: []uint64{20000}, // insert 80001.5
		BidRemoved: []uint32{2}, AskRemoved: []uint32{1}, Spread: &spread}
	server("server-l2-update", ServerFrame{Kind: KindL2Delta, Data: L2DeltaFrame{Update: &update}.Marshal()})
	// Replay the pair with the library's Book so the expected result is checked too.
	book := NewBook("BTC")
	if err := book.ApplyDelta(L2DeltaFrame{Snapshot: &snapshot}); err != nil {
		t.Fatal(err)
	}
	out[len(out)-2].BookAfter = bookObj(book)
	if err := book.ApplyDelta(L2DeltaFrame{Update: &update}); err != nil {
		t.Fatal(err)
	}
	out[len(out)-1].AppliesTo = "server-l2-snapshot"
	out[len(out)-1].BookAfter = bookObj(book)

	server("server-l2book", ServerFrame{Kind: KindL2Book, Data: L2BookFrame{Coin: "BTC", Time: 1792168695200, PxDecimals: 1, SzDecimals: 5,
		BidPx: []uint64{800000, 799990}, BidSz: []uint64{200000, 50000}, BidN: []uint32{2, 1},
		AskPx: []uint64{800010}, AskSz: []uint64{100000}, AskN: []uint32{1}}.Marshal()})
	bp, bs, ap, as := uint64(800000), uint64(200000), uint64(800010), uint64(100000)
	bn, an := uint32(2), uint32(1)
	server("server-bbo", ServerFrame{Kind: KindBBO, Data: BboFrame{Coin: "BTC", Time: 1792168695300, PxDecimals: 1, SzDecimals: 5,
		BidPx: &bp, BidSz: &bs, BidN: &bn, AskPx: &ap, AskSz: &as, AskN: &an}.Marshal()})
	server("server-bbo-no-ask", ServerFrame{Kind: KindBBO, Data: BboFrame{Coin: "BTC", Time: 1792168695400, PxDecimals: 1, SzDecimals: 5,
		BidPx: &bp, BidSz: &bs, BidN: &bn}.Marshal()})
	hash := bytes.Repeat([]byte{0xab}, 32)
	buyer, _ := hex.DecodeString("3333333333333333333333333333333333333333")
	seller, _ := hex.DecodeString("4444444444444444444444444444444444444444")
	server("server-trades", ServerFrame{Kind: KindTrades, Data: TradesFrame{Coin: "BTC", PxDecimals: 1, SzDecimals: 5, Trades: []Trade{
		{Buy: true, Px: 800010, Sz: 12000, Time: 1792168695500, TID: 9001, Hash: hash, Buyer: buyer, Seller: seller},
		{Buy: false, Px: 800000, Sz: 5000, Time: 1792168695600, TID: 9002, Hash: hash, Buyer: seller, Seller: buyer}}}.Marshal()})
	server("server-candle", ServerFrame{Kind: KindCandle, Data: CandleFrame{Coin: "BTC", Interval: "1m", OpenTime: 1792168680000, CloseTime: 1792168739999,
		PxDecimals: 1, SzDecimals: 5, Open: 800000, Close: 800010, High: 800050, Low: 799950, Volume: 345000, TradeCount: 17}.Marshal()})
	server("server-json-pong", ServerFrame{Kind: KindJSON, Data: []byte(`{"channel":"pong"}`)})
	server("server-json-post-reply", ServerFrame{ID: 7, Kind: KindJSON, Data: []byte(`{"channel":"post","data":{"id":7,"response":{"type":"info","payload":{"type":"openOrders","data":[]}}}}`)})
	return out
}

func (g goldenFrame) MarshalJSON() ([]byte, error) {
	o := obj{{"name", g.Name}, {"direction", g.Direction}, {"hex", g.Hex}}
	if g.AppliesTo != "" {
		o = append(o, kv{"applies_to", g.AppliesTo})
	}
	o = append(o, kv{"frame", g.Frame})
	if g.BookAfter != nil {
		o = append(o, kv{"book_after", g.BookAfter})
	}
	return o.MarshalJSON()
}

func TestFramesGolden(t *testing.T) {
	samples := sampleFrames(t)
	if os.Getenv("UPDATE_FRAMES") != "" {
		doc := obj{
			{"description", "Golden bin-ws frames. hex is one complete binary WebSocket message (WsV2ClientFrame or WsV2ServerFrame). frame is the decoded content using the field names of proto/binws.proto; bytes fields are lowercase hex in members ending in _hex. decimal shows fixed-point members as decimal strings (value = integer / 10^decimals). Every library must decode hex to exactly this content and, where it has an encoder, encode the content back to exactly this hex."},
			{"frames", samples},
		}
		raw, err := json.MarshalIndent(doc, "", "  ")
		if err != nil {
			t.Fatal(err)
		}
		if err := os.WriteFile(framesPath, append(raw, '\n'), 0o644); err != nil {
			t.Fatal(err)
		}
	}
	data, err := os.ReadFile(framesPath)
	if err != nil {
		t.Fatal(err)
	}
	var file struct {
		Frames []struct {
			Name      string          `json:"name"`
			Direction string          `json:"direction"`
			Hex       string          `json:"hex"`
			AppliesTo string          `json:"applies_to"`
			Frame     json.RawMessage `json:"frame"`
			BookAfter json.RawMessage `json:"book_after"`
		} `json:"frames"`
	}
	if err := json.Unmarshal(data, &file); err != nil {
		t.Fatal(err)
	}
	if len(file.Frames) != len(samples) {
		t.Fatalf("frames.json has %d frames, samples %d (run with UPDATE_FRAMES=1)", len(file.Frames), len(samples))
	}
	generic := func(v any) any {
		raw, err := json.Marshal(v)
		if err != nil {
			t.Fatal(err)
		}
		var out any
		if err := json.Unmarshal(raw, &out); err != nil {
			t.Fatal(err)
		}
		return out
	}
	books := map[string]*Book{}
	for _, f := range file.Frames {
		t.Run(f.Name, func(t *testing.T) {
			raw, err := hex.DecodeString(f.Hex)
			if err != nil {
				t.Fatal(err)
			}
			var described obj
			var reencoded []byte
			var server *ServerFrame
			if f.Direction == "client" {
				frame, err := UnmarshalClientFrame(raw)
				if err != nil {
					t.Fatal(err)
				}
				if described, err = describeClient(frame); err != nil {
					t.Fatal(err)
				}
				reencoded = frame.Marshal()
			} else {
				frame, err := UnmarshalServerFrame(raw)
				if err != nil {
					t.Fatal(err)
				}
				if described, err = describeServer(frame); err != nil {
					t.Fatal(err)
				}
				reencoded = frame.Marshal()
				server = &frame
			}
			var want any
			if err := json.Unmarshal(f.Frame, &want); err != nil {
				t.Fatal(err)
			}
			if got := generic(described); !reflect.DeepEqual(got, want) {
				t.Fatalf("decoded frame\n got %v\nwant %v", got, want)
			}
			if !bytes.Equal(reencoded, raw) {
				t.Fatalf("re-encoding changed the bytes:\n got %x\nwant %x", reencoded, raw)
			}
			if server != nil && server.Kind == KindL2Delta {
				delta, err := UnmarshalL2Delta(server.Data)
				if err != nil {
					t.Fatal(err)
				}
				book := books[f.AppliesTo]
				if book == nil {
					book = NewBook("BTC")
				}
				if err := book.ApplyDelta(delta); err != nil {
					t.Fatal(err)
				}
				books[f.Name] = book
				var wantBook any
				if err := json.Unmarshal(f.BookAfter, &wantBook); err != nil {
					t.Fatal(err)
				}
				if got := generic(bookObj(book)); !reflect.DeepEqual(got, wantBook) {
					t.Fatalf("book after frame\n got %v\nwant %v", got, wantBook)
				}
			}
		})
	}
}
