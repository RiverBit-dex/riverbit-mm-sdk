// Package binws is a client for the RiverBit binary WebSocket endpoint
// (wss://<host>/bin-ws). Every message is one Protobuf frame: a
// WsV2ClientFrame from the client, a WsV2ServerFrame from the server. The
// definitions are in proto/binws.proto of this repository; the codec here is
// hand-written, so no protoc or generated code is needed.
//
// The package covers the frames and their codec, the EIP-191 login, a Session
// (login, subscribe, ping, requests matched by frame id), a Run loop that
// reconnects, re-logs in and resubscribes, typed-frame routing, and a Book
// that replays the incremental l2 stream. Signed actions are built and signed
// by package exchange, which uses a Session as its transport.
//
// A market data session:
//
//	signer, _ := exchange.NewSigner(keyHex)
//	book := binws.NewBook("BTC")
//	router := &binws.Router{
//		Book: book,
//		OnBook: func(b *binws.Book, snapshot bool) {
//			if bid, ask := b.Best(); bid != nil && ask != nil {
//				fmt.Println(bid.PxText, ask.PxText)
//			}
//		},
//		OnBBO: func(f binws.BboFrame) { /* decode with binws.FormatFixed */ },
//	}
//	session, err := binws.Dial(ctx, binws.Config{
//		URL:       "wss://pre-api.riverbit.io/bin-ws",
//		LoginHost: "pre-api.riverbit.io",
//		Sign:      signer,
//		OnFrame:   router.Handle,
//	})
//	if err != nil {
//		return err
//	}
//	defer session.Close()
//	session.Subscribe(binws.L2("BTC"))
//	<-session.Done()
//
// Prices and sizes are fixed-point integers on the wire (value =
// integer / 10^decimals). FormatFixed renders them as exact decimal strings;
// Book keeps both the exact value and the text.
package binws
