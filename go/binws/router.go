package binws

import (
	"encoding/json"
	"fmt"
)

// Router dispatches server frames to typed callbacks. Router.Handle fits
// Config.OnFrame. It runs on the session's read goroutine: keep callbacks
// fast, and do not call Session methods that wait for a reply from them
// (Subscribe, Unsubscribe and Ping only write and are fine). Every field is
// optional; frames without a callback are dropped.
type Router struct {
	// Book, if set, replays the incremental "l2" stream: typed l2_delta frames
	// and the JSON fallback of the same channel are applied to it.
	Book *Book
	// OnBook is called after a frame was applied to Book. snapshot is true when
	// the frame replaced the whole book.
	OnBook func(book *Book, snapshot bool)
	// Resync is called when an l2 frame cannot be applied (a gap, a decimals
	// change, a frame before any snapshot). Book has been cleared; unsubscribe
	// and resubscribe to the l2 stream to get a new snapshot.
	Resync func()

	OnL2Book func(L2BookFrame)
	OnBBO    func(BboFrame)
	OnCandle func(CandleFrame)
	OnTrades func(TradesFrame)
	// OnJSON receives every JSON frame except the "l2" channel when Book is
	// set: pushes of the account streams, subscription acknowledgments, pongs,
	// errors, and the JSON fallbacks of the typed streams. channel is the
	// frame's "channel" member and data its "data" member.
	OnJSON func(channel string, data json.RawMessage, frame ServerFrame)
	// OnError receives undecodable frames and l2 frames that could not be applied.
	OnError func(error)
}

// Handle dispatches one server frame.
func (r *Router) Handle(f ServerFrame) {
	switch f.Kind {
	case KindJSON:
		r.handleJSON(f)
	case KindL2Delta:
		d, err := UnmarshalL2Delta(f.Data)
		if err != nil {
			r.applyFailed(err)
			return
		}
		r.applyBook(func(b *Book) error { return b.ApplyDelta(d) }, d.Snapshot != nil)
	case KindL2Book:
		if b, err := UnmarshalL2Book(f.Data); err != nil {
			r.fail(fmt.Errorf("bad l2Book frame: %w", err))
		} else if r.OnL2Book != nil {
			r.OnL2Book(b)
		}
	case KindBBO:
		if b, err := UnmarshalBBO(f.Data); err != nil {
			r.fail(fmt.Errorf("bad bbo frame: %w", err))
		} else if r.OnBBO != nil {
			r.OnBBO(b)
		}
	case KindCandle:
		if c, err := UnmarshalCandle(f.Data); err != nil {
			r.fail(fmt.Errorf("bad candle frame: %w", err))
		} else if r.OnCandle != nil {
			r.OnCandle(c)
		}
	case KindTrades:
		if t, err := UnmarshalTrades(f.Data); err != nil {
			r.fail(fmt.Errorf("bad trades frame: %w", err))
		} else if r.OnTrades != nil {
			r.OnTrades(t)
		}
	default:
		r.fail(fmt.Errorf("unexpected %s frame id=%d", f.Kind, f.ID))
	}
}

func (r *Router) fail(err error) {
	if r.OnError != nil {
		r.OnError(err)
	}
}

func (r *Router) handleJSON(f ServerFrame) {
	var frame struct {
		Channel string          `json:"channel"`
		Data    json.RawMessage `json:"data"`
	}
	if err := json.Unmarshal(f.Data, &frame); err != nil {
		r.fail(fmt.Errorf("invalid json frame: %w", err))
		return
	}
	if frame.Channel == "l2" && r.Book != nil {
		r.applyBook(func(b *Book) error { return b.ApplyJSON(frame.Data) }, false)
		return
	}
	if r.OnJSON != nil {
		r.OnJSON(frame.Channel, frame.Data, f)
	}
}

func (r *Router) applyFailed(err error) { r.applyBook(func(*Book) error { return err }, false) }

func (r *Router) applyBook(apply func(*Book) error, snapshot bool) {
	if r.Book == nil {
		r.fail(fmt.Errorf("l2 frame without a Book"))
		return
	}
	if err := apply(r.Book); err != nil {
		*r.Book = *NewBook(r.Book.Coin)
		r.fail(fmt.Errorf("l2 book out of sync: %w", err))
		if r.Resync != nil {
			r.Resync()
		}
		return
	}
	if r.OnBook != nil {
		r.OnBook(r.Book, snapshot)
	}
}
