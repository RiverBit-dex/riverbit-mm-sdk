package main

import (
	"context"
	"encoding/hex"
	"encoding/json"
	"fmt"
	"log/slog"
	"time"

	"github.com/riverbit-dex/riverbit-mm-sdk/go/binws"
)

// Subscriptions lists what the example subscribes to: the order book (`l2`
// incremental by default, or `l2Book`), trades, 1m candles and, when user is
// set, orderUpdates and userFills for that account (master or vault).
func Subscriptions(coin, bookStream, user string) []binws.Subscription {
	book := binws.L2(coin)
	if bookStream == "l2Book" {
		book = binws.L2Book(coin)
	}
	subs := []binws.Subscription{book, binws.Trades(coin), binws.Candle(coin, "1m")}
	if user != "" {
		subs = append(subs, binws.OrderUpdates(user), binws.UserFills(user))
	}
	return subs
}

// OrderSubscriptions are the account event streams only (used by `trade`).
func OrderSubscriptions(user string) []binws.Subscription {
	return []binws.Subscription{binws.OrderUpdates(user), binws.UserFills(user)}
}

type orderEvent struct {
	Order struct {
		OID   uint64 `json:"oid"`
		Coin  string `json:"coin"`
		Side  string `json:"side"`
		Price string `json:"limitPx"`
		Size  string `json:"sz"`
		Cloid string `json:"cloid"`
	} `json:"order"`
	Status string `json:"status"`
}

type jsonTrade struct {
	Coin  string `json:"coin"`
	Side  string `json:"side"`
	Price string `json:"px"`
	Size  string `json:"sz"`
	Time  int64  `json:"time"`
}

type jsonLevel struct {
	Price string `json:"px"`
	Size  string `json:"sz"`
	N     int    `json:"n"`
}

// Handler logs server frames with slog. It is safe to use as
// binws.Config.OnFrame: it runs on the session's read goroutine and never
// waits on the connection. It is a thin logging layer over binws.Router.
type Handler struct {
	Log *slog.Logger
	// Book replays the `l2` subscription; nil when not subscribed to `l2`.
	Book *binws.Book
	// Resync is called when an `l2` frame cannot be applied (gap, decimals
	// change): the book is cleared and should be re-subscribed.
	Resync func()

	router *binws.Router
}

// Handle dispatches one frame.
func (h *Handler) Handle(f binws.ServerFrame) {
	if h.router == nil {
		h.router = &binws.Router{
			Book:     h.Book,
			OnBook:   h.logBook,
			Resync:   func() { h.callResync() },
			OnL2Book: h.logL2Book,
			OnBBO:    h.logBBO,
			OnCandle: h.logCandle,
			OnTrades: h.logTrades,
			OnJSON:   h.logJSON,
			OnError:  func(err error) { h.Log.Warn("frame problem", "error", err) },
		}
	}
	h.router.Handle(f)
}

func (h *Handler) callResync() {
	if h.Resync != nil {
		h.Resync()
	}
}

func (h *Handler) logBook(b *binws.Book, snapshot bool) {
	bid, ask := b.Best()
	attrs := []any{"coin", b.Coin, "time_ms", b.Time, "snapshot", snapshot, "bid_levels", len(b.Bids), "ask_levels", len(b.Asks)}
	if bid != nil {
		attrs = append(attrs, "bid1_px", bid.PxText, "bid1_sz", bid.SzText)
	}
	if ask != nil {
		attrs = append(attrs, "ask1_px", ask.PxText, "ask1_sz", ask.SzText)
	}
	h.Log.Info("l2", attrs...)
}

func (h *Handler) logL2Book(b binws.L2BookFrame) {
	attrs := []any{"coin", b.Coin, "time_ms", b.Time, "bid_levels", len(b.BidPx), "ask_levels", len(b.AskPx)}
	if len(b.BidPx) > 0 {
		attrs = append(attrs, "best_bid_px", binws.FormatFixed(b.BidPx[0], b.PxDecimals), "best_bid_sz", binws.FormatFixed(b.BidSz[0], b.SzDecimals))
	}
	if len(b.AskPx) > 0 {
		attrs = append(attrs, "best_ask_px", binws.FormatFixed(b.AskPx[0], b.PxDecimals), "best_ask_sz", binws.FormatFixed(b.AskSz[0], b.SzDecimals))
	}
	h.Log.Info("l2Book", attrs...)
}

func (h *Handler) logBBO(b binws.BboFrame) {
	attrs := []any{"coin", b.Coin, "time_ms", b.Time}
	if b.BidPx != nil && b.BidSz != nil {
		attrs = append(attrs, "bid_px", binws.FormatFixed(*b.BidPx, b.PxDecimals), "bid_sz", binws.FormatFixed(*b.BidSz, b.SzDecimals))
	}
	if b.AskPx != nil && b.AskSz != nil {
		attrs = append(attrs, "ask_px", binws.FormatFixed(*b.AskPx, b.PxDecimals), "ask_sz", binws.FormatFixed(*b.AskSz, b.SzDecimals))
	}
	h.Log.Info("bbo", attrs...)
}

func (h *Handler) logCandle(c binws.CandleFrame) {
	px := func(v uint64) string { return binws.FormatFixed(v, c.PxDecimals) }
	h.Log.Info("candle", "coin", c.Coin, "interval", c.Interval, "open_time_ms", c.OpenTime, "close_time_ms", c.CloseTime,
		"o", px(c.Open), "h", px(c.High), "l", px(c.Low), "c", px(c.Close),
		"v", binws.FormatFixed(c.Volume, c.SzDecimals), "n", c.TradeCount)
}

func (h *Handler) logTrades(t binws.TradesFrame) {
	for _, tr := range t.Trades {
		side := "sell"
		if tr.Buy {
			side = "buy"
		}
		h.Log.Info("trade", "coin", t.Coin, "side", side, "px", binws.FormatFixed(tr.Px, t.PxDecimals),
			"sz", binws.FormatFixed(tr.Sz, t.SzDecimals), "time_ms", tr.Time, "tid", tr.TID,
			"buyer", hexOrEmpty(tr.Buyer), "seller", hexOrEmpty(tr.Seller))
	}
}

func hexOrEmpty(b []byte) string {
	if len(b) == 0 {
		return ""
	}
	return "0x" + hex.EncodeToString(b)
}

// logJSON handles the JSON frames: the account streams and the JSON fallbacks
// of the typed market data streams.
func (h *Handler) logJSON(channel string, data json.RawMessage, f binws.ServerFrame) {
	switch channel {
	case "l2Book":
		var book struct {
			Coin   string         `json:"coin"`
			Time   int64          `json:"time"`
			Levels [2][]jsonLevel `json:"levels"`
		}
		if err := json.Unmarshal(data, &book); err != nil {
			h.Log.Warn("invalid l2Book json", "error", err)
			return
		}
		attrs := []any{"coin", book.Coin, "time_ms", book.Time}
		if len(book.Levels[0]) > 0 {
			attrs = append(attrs, "best_bid_px", book.Levels[0][0].Price, "best_bid_sz", book.Levels[0][0].Size)
		}
		if len(book.Levels[1]) > 0 {
			attrs = append(attrs, "best_ask_px", book.Levels[1][0].Price, "best_ask_sz", book.Levels[1][0].Size)
		}
		h.Log.Info("l2Book (json)", attrs...)
	case "trades":
		var trades []jsonTrade
		if err := json.Unmarshal(data, &trades); err != nil {
			h.Log.Warn("invalid trades json", "error", err)
			return
		}
		for _, t := range trades {
			h.Log.Info("trade (json)", "coin", t.Coin, "side", t.Side, "px", t.Price, "sz", t.Size, "time_ms", t.Time)
		}
	case "candle", "bbo":
		h.Log.Info(channel+" (json)", "data", string(data))
	case "orderUpdates":
		var events []orderEvent
		if err := json.Unmarshal(data, &events); err != nil {
			h.Log.Warn("invalid orderUpdates json", "error", err)
			return
		}
		for _, e := range events {
			h.Log.Info("order update", "oid", e.Order.OID, "cloid", e.Order.Cloid, "coin", e.Order.Coin, "side", e.Order.Side,
				"px", e.Order.Price, "sz", e.Order.Size, "status", e.Status)
		}
	case "userFills", "openOrders":
		h.Log.Info(channel, "data", truncate(data, 2000))
	case "subscriptionResponse":
		h.Log.Info("subscribed", "data", truncate(data, 300))
	case "pong":
	case "error":
		h.Log.Warn("server error", "data", truncate(data, 500))
	case "post":
		h.Log.Warn("reply without a waiting request (late or timed out)", "id", f.ID, "data", truncate(data, 500))
	default:
		h.Log.Info("json frame", "channel", channel, "data", truncate(data, 500))
	}
}

func truncate(b []byte, n int) string {
	if len(b) <= n {
		return string(b)
	}
	return string(b[:n]) + "..."
}

// RunConfig configures the market data loop.
type RunConfig struct {
	URL        string
	LoginHost  string
	Sign       binws.Signer
	Coin       string
	BookStream string // "l2" or "l2Book"
	// User is the account whose orderUpdates/userFills are printed: the vault
	// when trading for one. Empty = the master account resolved by login.
	User string
	Log  *slog.Logger
}

// Run keeps one bin-ws session alive (reconnect -> login -> subscribe) until
// ctx is canceled, using binws.Run.
func Run(ctx context.Context, rc RunConfig) error {
	var h *Handler
	var session *binws.Session
	ready := make(chan struct{})
	newConfig := func() binws.Config {
		// Fresh state for every connection.
		h = &Handler{Log: rc.Log}
		if rc.BookStream != "l2Book" {
			h.Book = binws.NewBook(rc.Coin)
		}
		ready = make(chan struct{})
		current := ready
		h.Resync = func() {
			<-current
			s := session
			sub := binws.L2(rc.Coin)
			_ = s.Unsubscribe(sub)
			if err := s.Subscribe(sub); err != nil {
				rc.Log.Warn("l2 resubscribe failed", "error", err)
			}
		}
		return binws.Config{
			URL: rc.URL, LoginHost: rc.LoginHost, Sign: rc.Sign, OnFrame: h.Handle,
			OnDecodeError: func(err error) { rc.Log.Warn("undecodable server frame", "error", err) },
		}
	}
	setup := func(s *binws.Session) error {
		session = s
		close(ready)
		LogLogin(rc.Log, s)
		user := rc.User
		if user == "" {
			user = s.User()
		}
		for _, sub := range Subscriptions(rc.Coin, rc.BookStream, user) {
			if err := s.Subscribe(sub); err != nil {
				return fmt.Errorf("subscribe %s: %w", sub.Type, err)
			}
		}
		rc.Log.Info("subscribed to bin-ws streams", "coin", rc.Coin, "book", rc.BookStream, "orders_user", user, "url", rc.URL)
		return nil
	}
	onEnd := func(err error, retryIn time.Duration) {
		rc.Log.Warn("bin-ws connection ended", "error", err, "retry_in", retryIn)
	}
	return binws.Run(ctx, newConfig, setup, onEnd)
}

// LogLogin prints who the server recognized.
func LogLogin(log *slog.Logger, s *binws.Session) {
	role := "API wallet"
	if s.User() == s.Signer() {
		role = "master key"
	}
	log.Info("bin-ws login ok", "user", s.User(), "signer", s.Signer(), "signer_role", role)
}
