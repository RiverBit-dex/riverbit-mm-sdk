//! Decodes and re-encodes every frame of `testdata/frames.json` and replays
//! the `l2` frames into a book.

use riverbit_mm_sdk::decimal::format_fixed;
use riverbit_mm_sdk::proto::{ClientBody, ClientFrame, L2DeltaFrame, ServerBody, ServerFrame};
use riverbit_mm_sdk::{Book, Frame};
use serde_json::{json, Map, Value};

fn golden() -> Value {
    let path = concat!(env!("CARGO_MANIFEST_DIR"), "/../testdata/frames.json");
    serde_json::from_str(&std::fs::read_to_string(path).expect("read frames.json"))
        .expect("parse frames.json")
}

fn hx(bytes: &[u8]) -> String {
    hex::encode(bytes)
}

fn text(bytes: &[u8]) -> String {
    String::from_utf8(bytes.to_vec()).unwrap()
}

/// `{k: [..]}` for a repeated member; absent optional members are left out by the caller.
fn nums<T: Copy + Into<u64>>(values: &[T]) -> Value {
    Value::Array(values.iter().map(|v| json!((*v).into())).collect())
}

fn dec(values: &[u64], decimals: u32) -> Value {
    Value::Array(
        values
            .iter()
            .map(|v| json!(format_fixed(*v, decimals)))
            .collect(),
    )
}

/// Returns (body name, json text, message, decimal) for a client frame.
fn describe_client(
    frame: &ClientFrame,
) -> (&'static str, Option<String>, Option<Value>, Option<Value>) {
    match &frame.body {
        ClientBody::InfoJson(b) => ("info_json", Some(text(b)), None, None),
        ClientBody::SubscribeJson(b) => ("subscribe_json", Some(text(b)), None, None),
        ClientBody::UnsubscribeJson(b) => ("unsubscribe_json", Some(text(b)), None, None),
        ClientBody::Ping => ("ping", None, None, None),
        ClientBody::Login(l) => (
            "login",
            None,
            Some(
                json!({"timestamp_ms": l.timestamp_ms, "nonce_hex": hx(&l.nonce), "signature_hex": hx(&l.signature)}),
            ),
            None,
        ),
    }
}

fn describe_server(
    frame: &ServerFrame,
) -> (&'static str, Option<String>, Option<Value>, Option<Value>) {
    match &frame.body {
        ServerBody::Json(b) => ("json", Some(text(b)), None, None),
        ServerBody::Login(r) => (
            "login",
            None,
            Some(
                json!({"ok": r.ok, "user_hex": hx(&r.user), "signer_hex": hx(&r.signer), "error": r.error}),
            ),
            None,
        ),
        ServerBody::L2Book(b) => (
            "l2_book",
            None,
            Some(json!({
                "coin": b.coin, "time": b.time, "px_decimals": b.px_decimals, "sz_decimals": b.sz_decimals,
                "bid_px": b.bid_px, "bid_sz": b.bid_sz, "bid_n": nums(&b.bid_n),
                "ask_px": b.ask_px, "ask_sz": b.ask_sz, "ask_n": nums(&b.ask_n),
            })),
            Some(json!({
                "bid_px": dec(&b.bid_px, b.px_decimals), "bid_sz": dec(&b.bid_sz, b.sz_decimals),
                "ask_px": dec(&b.ask_px, b.px_decimals), "ask_sz": dec(&b.ask_sz, b.sz_decimals),
            })),
        ),
        ServerBody::L2Delta(L2DeltaFrame::Snapshot(b)) => (
            "l2_delta.snapshot",
            None,
            Some(json!({
                "coin": b.coin, "time": b.time, "px_decimals": b.px_decimals, "sz_decimals": b.sz_decimals,
                "bid_px": b.bid_px, "bid_sz": b.bid_sz, "bid_n": nums(&b.bid_n),
                "ask_px": b.ask_px, "ask_sz": b.ask_sz, "ask_n": nums(&b.ask_n),
            })),
            Some(json!({
                "bid_px": dec(&b.bid_px, b.px_decimals), "bid_sz": dec(&b.bid_sz, b.sz_decimals),
                "ask_px": dec(&b.ask_px, b.px_decimals), "ask_sz": dec(&b.ask_sz, b.sz_decimals),
            })),
        ),
        ServerBody::L2Delta(L2DeltaFrame::Update(u)) => {
            let mut message = json!({
                "coin": u.coin, "time": u.time, "px_decimals": u.px_decimals, "sz_decimals": u.sz_decimals,
                "bid_px": u.bid_px, "bid_sz": u.bid_sz, "ask_px": u.ask_px, "ask_sz": u.ask_sz,
                "bid_removed": nums(&u.bid_removed), "ask_removed": nums(&u.ask_removed),
            });
            let mut decimal = json!({
                "bid_px": dec(&u.bid_px, u.px_decimals), "bid_sz": dec(&u.bid_sz, u.sz_decimals),
                "ask_px": dec(&u.ask_px, u.px_decimals), "ask_sz": dec(&u.ask_sz, u.sz_decimals),
            });
            if let Some(s) = u.spread {
                message["spread"] = json!(s);
                decimal["spread"] = json!(format_fixed(s, u.px_decimals));
            }
            ("l2_delta.update", None, Some(message), Some(decimal))
        }
        ServerBody::Bbo(b) => {
            let mut message = Map::new();
            let mut decimal = Map::new();
            message.insert("coin".into(), json!(b.coin));
            message.insert("time".into(), json!(b.time));
            message.insert("px_decimals".into(), json!(b.px_decimals));
            message.insert("sz_decimals".into(), json!(b.sz_decimals));
            for (name, value, decimals) in [
                ("bid_px", b.bid_px, Some(b.px_decimals)),
                ("bid_sz", b.bid_sz, Some(b.sz_decimals)),
                ("bid_n", b.bid_n.map(u64::from), None),
                ("ask_px", b.ask_px, Some(b.px_decimals)),
                ("ask_sz", b.ask_sz, Some(b.sz_decimals)),
                ("ask_n", b.ask_n.map(u64::from), None),
            ] {
                if let Some(v) = value {
                    message.insert(name.into(), json!(v));
                    if let Some(d) = decimals {
                        decimal.insert(name.into(), json!(format_fixed(v, d)));
                    }
                }
            }
            (
                "bbo",
                None,
                Some(Value::Object(message)),
                Some(Value::Object(decimal)),
            )
        }
        ServerBody::Candle(c) => (
            "candle",
            None,
            Some(json!({
                "coin": c.coin, "interval": c.interval, "open_time": c.open_time, "close_time": c.close_time,
                "px_decimals": c.px_decimals, "sz_decimals": c.sz_decimals,
                "open": c.open, "close": c.close, "high": c.high, "low": c.low,
                "volume": c.volume, "trade_count": c.trade_count,
            })),
            Some(json!({
                "open": format_fixed(c.open, c.px_decimals), "close": format_fixed(c.close, c.px_decimals),
                "high": format_fixed(c.high, c.px_decimals), "low": format_fixed(c.low, c.px_decimals),
                "volume": format_fixed(c.volume, c.sz_decimals),
            })),
        ),
        ServerBody::Trades(t) => (
            "trades",
            None,
            Some(json!({
                "coin": t.coin, "px_decimals": t.px_decimals, "sz_decimals": t.sz_decimals,
                "trades": t.trades.iter().map(|x| json!({
                    "buy": x.buy, "px": x.px, "sz": x.sz, "time": x.time, "tid": x.tid,
                    "hash_hex": hx(&x.hash), "buyer_hex": hx(&x.buyer), "seller_hex": hx(&x.seller),
                })).collect::<Vec<_>>(),
            })),
            Some(json!({
                "trades": t.trades.iter().map(|x| json!({
                    "px": format_fixed(x.px, t.px_decimals), "sz": format_fixed(x.sz, t.sz_decimals),
                })).collect::<Vec<_>>(),
            })),
        ),
    }
}

fn book_levels(book: &Book) -> Value {
    let side = |levels: &[riverbit_mm_sdk::book::Level]| -> Vec<Value> {
        levels
            .iter()
            .map(|l| json!({"px": l.px, "sz": l.sz}))
            .collect()
    };
    json!({"bids": side(book.bids()), "asks": side(book.asks())})
}

#[test]
fn every_golden_frame_decodes_to_its_content_and_encodes_back_to_its_hex() {
    let golden = golden();
    let frames = golden["frames"].as_array().unwrap();
    assert!(!frames.is_empty());
    for entry in frames {
        let name = entry["name"].as_str().unwrap();
        let bytes = hex::decode(entry["hex"].as_str().unwrap()).unwrap();
        let expected = &entry["frame"];
        let (id, body, json_text, message, decimal, encoded) = match entry["direction"]
            .as_str()
            .unwrap()
        {
            "client" => {
                let frame = ClientFrame::decode(&bytes).unwrap_or_else(|e| panic!("{name}: {e}"));
                let (body, j, m, d) = describe_client(&frame);
                (frame.id, body, j, m, d, frame.encode())
            }
            "server" => {
                let frame = ServerFrame::decode(&bytes).unwrap_or_else(|e| panic!("{name}: {e}"));
                let (body, j, m, d) = describe_server(&frame);
                (frame.id, body, j, m, d, frame.encode())
            }
            other => panic!("{name}: direction {other}"),
        };
        assert_eq!(id, expected["id"].as_u64().unwrap(), "{name}: id");
        assert_eq!(body, expected["body"].as_str().unwrap(), "{name}: body");
        assert_eq!(
            json_text.as_deref(),
            expected["json"].as_str(),
            "{name}: json"
        );
        assert_eq!(message.as_ref(), expected.get("message"), "{name}: message");
        assert_eq!(decimal.as_ref(), expected.get("decimal"), "{name}: decimal");
        assert_eq!(
            hx(&encoded),
            entry["hex"].as_str().unwrap(),
            "{name}: re-encoded hex"
        );
    }
}

#[test]
fn l2_golden_frames_replay_into_the_expected_books() {
    let golden = golden();
    let frames = golden["frames"].as_array().unwrap();
    let find = |name: &str| {
        frames
            .iter()
            .find(|f| f["name"] == name)
            .unwrap_or_else(|| panic!("no frame {name}"))
    };
    let mut replayed = 0;
    for entry in frames.iter().filter(|f| f.get("book_after").is_some()) {
        let name = entry["name"].as_str().unwrap();
        let mut book = Book::new("BTC");
        // An update changes the book that its `applies_to` frame produced.
        let mut chain = Vec::new();
        let mut current = entry;
        loop {
            chain.push(current);
            match current.get("applies_to").and_then(Value::as_str) {
                Some(parent) => current = find(parent),
                None => break,
            }
        }
        chain.reverse();
        for step in chain {
            let bytes = hex::decode(step["hex"].as_str().unwrap()).unwrap();
            let frame = Frame::from_server(ServerFrame::decode(&bytes).unwrap()).unwrap();
            assert!(
                book.apply_frame(&frame).unwrap(),
                "{name}: l2 frame applied"
            );
            assert_eq!(
                book_levels(&book),
                step["book_after"],
                "{name}: book after {}",
                step["name"]
            );
            replayed += 1;
        }
    }
    assert!(
        replayed >= 2,
        "the snapshot and the update were both replayed"
    );
}
