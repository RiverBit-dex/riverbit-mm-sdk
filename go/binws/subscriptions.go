package binws

// Subscription is a bin-ws subscription object, sent as subscribe_json. Field
// order is fixed so the wire JSON is deterministic. Use the constructors for
// the streams that have typed frames, or fill the struct for other streams
// listed in the protocol guide.
type Subscription struct {
	Type     string `json:"type"`
	C        string `json:"c,omitempty"` // the incremental "l2" stream names the coin "c"
	Coin     string `json:"coin,omitempty"`
	Interval string `json:"interval,omitempty"`
	User     string `json:"user,omitempty"`
}

// L2 subscribes to the incremental order book of coin: a snapshot, then
// updates (typed l2_delta frames).
func L2(coin string) Subscription { return Subscription{Type: "l2", C: coin} }

// L2Book subscribes to full order book snapshots of coin.
func L2Book(coin string) Subscription { return Subscription{Type: "l2Book", Coin: coin} }

// BBO subscribes to the best bid and offer of coin.
func BBO(coin string) Subscription { return Subscription{Type: "bbo", Coin: coin} }

// Trades subscribes to the trades of coin.
func Trades(coin string) Subscription { return Subscription{Type: "trades", Coin: coin} }

// Candle subscribes to the candles of coin for interval ("1m", "5m", "1h", ...).
func Candle(coin, interval string) Subscription {
	return Subscription{Type: "candle", Coin: coin, Interval: interval}
}

// OrderUpdates subscribes to the order status changes of user, a master
// account or a vault it leads. Updates only cover changes after the
// subscription; read openOrders for the initial state.
func OrderUpdates(user string) Subscription { return Subscription{Type: "orderUpdates", User: user} }

// UserFills subscribes to the fills of user, a master account or a vault it leads.
func UserFills(user string) Subscription { return Subscription{Type: "userFills", User: user} }

// OpenOrders subscribes to the open orders of user, a master account or a
// vault it leads.
func OpenOrders(user string) Subscription { return Subscription{Type: "openOrders", User: user} }
