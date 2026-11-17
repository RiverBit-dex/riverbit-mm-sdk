package binws

import (
	"encoding/json"
	"strings"
	"testing"
)

func TestSubscriptionObjects(t *testing.T) {
	cases := map[string]Subscription{
		`{"type":"l2","c":"BTC"}`:                        L2("BTC"),
		`{"type":"l2Book","coin":"BTC"}`:                 L2Book("BTC"),
		`{"type":"bbo","coin":"BTC"}`:                    BBO("BTC"),
		`{"type":"trades","coin":"BTC"}`:                 Trades("BTC"),
		`{"type":"candle","coin":"BTC","interval":"1m"}`: Candle("BTC", "1m"),
		`{"type":"orderUpdates","user":"0xabc"}`:         OrderUpdates("0xabc"),
		`{"type":"userFills","user":"0xabc"}`:            UserFills("0xabc"),
		`{"type":"openOrders","user":"0xabc"}`:           OpenOrders("0xabc"),
	}
	for want, sub := range cases {
		if got, _ := json.Marshal(sub); string(got) != want {
			t.Errorf("subscription = %s, want %s", got, want)
		}
	}
}

func TestRouterDispatchesTypedAndJSONFrames(t *testing.T) {
	var books, bbos, candles, trades, l2books, resyncs int
	var jsonChannels []string
	var errs []error
	r := &Router{
		Book:     NewBook("RBT"),
		OnBook:   func(*Book, bool) { books++ },
		Resync:   func() { resyncs++ },
		OnL2Book: func(L2BookFrame) { l2books++ },
		OnBBO:    func(BboFrame) { bbos++ },
		OnCandle: func(CandleFrame) { candles++ },
		OnTrades: func(TradesFrame) { trades++ },
		OnJSON:   func(channel string, _ json.RawMessage, _ ServerFrame) { jsonChannels = append(jsonChannels, channel) },
		OnError:  func(err error) { errs = append(errs, err) },
	}
	snap := L2BookFrame{Coin: "RBT", PxDecimals: 1, SzDecimals: 2, BidPx: []uint64{1000}, BidSz: []uint64{5}, AskPx: []uint64{1010}, AskSz: []uint64{7}}
	r.Handle(ServerFrame{Kind: KindL2Delta, Data: L2DeltaFrame{Snapshot: &snap}.Marshal()})
	r.Handle(ServerFrame{Kind: KindL2Book, Data: snap.Marshal()})
	r.Handle(ServerFrame{Kind: KindBBO, Data: BboFrame{Coin: "RBT"}.Marshal()})
	r.Handle(ServerFrame{Kind: KindCandle, Data: CandleFrame{Coin: "RBT"}.Marshal()})
	r.Handle(ServerFrame{Kind: KindTrades, Data: TradesFrame{Coin: "RBT"}.Marshal()})
	// The JSON fallback of the l2 channel feeds the same book, not OnJSON.
	r.Handle(ServerFrame{Kind: KindJSON, Data: []byte(`{"channel":"l2","data":{"u":{"c":"RBT","t":2,"l":[[{"p":"100.5","s":"4"}],[]],"r":[[],[]]}}}`)})
	r.Handle(ServerFrame{Kind: KindJSON, Data: []byte(`{"channel":"orderUpdates","data":[]}`)})
	if books != 2 || l2books != 1 || bbos != 1 || candles != 1 || trades != 1 || strings.Join(jsonChannels, ",") != "orderUpdates" || len(errs) != 0 {
		t.Fatalf("books=%d l2books=%d bbos=%d candles=%d trades=%d json=%v errs=%v", books, l2books, bbos, candles, trades, jsonChannels, errs)
	}
	if bid, _ := r.Book.Best(); bid == nil || bid.PxText != "100.5" {
		t.Fatalf("book after the JSON update: %+v", r.Book.Bids)
	}
	// An update that does not fit clears the book and asks to resubscribe.
	bad := L2DeltaUpdate{Coin: "RBT", PxDecimals: 1, SzDecimals: 2, BidRemoved: []uint32{9}}
	r.Handle(ServerFrame{Kind: KindL2Delta, Data: L2DeltaFrame{Update: &bad}.Marshal()})
	if resyncs != 1 || r.Book.HasBook || len(errs) != 1 {
		t.Fatalf("resyncs=%d hasBook=%v errs=%v", resyncs, r.Book.HasBook, errs)
	}
	// A frame that does not decode also invalidates the book.
	r.Handle(ServerFrame{Kind: KindL2Delta, Data: L2DeltaFrame{Snapshot: &snap}.Marshal()})
	r.Handle(ServerFrame{Kind: KindL2Delta, Data: []byte{0xff}})
	if resyncs != 2 || r.Book.HasBook {
		t.Fatalf("malformed delta kept the book: resyncs=%d", resyncs)
	}
}
