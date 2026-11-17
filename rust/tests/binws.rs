//! The bin-ws session against an in-process server.

mod common;

use std::sync::Arc;
use std::time::Duration;

use common::{
    http_server, next_frame, next_seen, ws_server, HttpReply, Seen, WsServer, WsServerOptions,
    TEST_KEY,
};
use riverbit_mm_sdk::actions::{Order, Place, Tif};
use riverbit_mm_sdk::error::UnknownReason;
use riverbit_mm_sdk::proto::{
    BboFrame, CandleFrame, ClientBody, L2BookFrame, L2DeltaFrame, L2DeltaUpdate, ServerBody,
    ServerFrame, Trade, TradesFrame,
};
use riverbit_mm_sdk::signing::{Address, SigningDomain};
use riverbit_mm_sdk::{
    BinWsConfig, BinWsSession, Book, Cloid, Error, Event, Events, ExchangeClient, Frame,
    InfoClient, ReconnectPolicy, Signer, Subscription,
};
use serde_json::json;

const HOST: &str = "pre-api.example";

fn signer() -> Signer {
    Signer::from_hex(TEST_KEY).unwrap()
}

fn master() -> Address {
    Address::from_bytes([0x22; 20])
}

async fn server() -> WsServer {
    ws_server(WsServerOptions {
        login_host: HOST.into(),
        user: master(),
        reject: None,
    })
    .await
}

fn config(server: &WsServer) -> BinWsConfig {
    let mut cfg = BinWsConfig::new(server.url.clone(), HOST, signer());
    cfg.reconnect = Some(ReconnectPolicy {
        initial_backoff: Duration::from_millis(50),
        max_backoff: Duration::from_millis(200),
        max_attempts: None,
    });
    cfg
}

async fn connect(server: &WsServer) -> (BinWsSession, Events) {
    BinWsSession::connect(config(server))
        .await
        .expect("connect")
}

async fn next_event(events: &mut Events) -> Event {
    tokio::time::timeout(Duration::from_secs(5), events.next())
        .await
        .expect("timed out waiting for an event")
        .expect("session ended")
}

fn post_reply(id: u64, kind: &str, payload: &str) -> String {
    format!(
        r#"{{"channel":"post","data":{{"id":{id},"response":{{"type":"{kind}","payload":{payload}}}}}}}"#
    )
}

fn place_one() -> Place {
    Place::new(vec![
        Order::limit(1, true, "80000", "0.001", Tif::Alo).with_cloid(Cloid::from_u128(1))
    ])
}

const RESTING: &str =
    r#"{"status":"ok","response":{"type":"order","data":{"statuses":[{"resting":{"oid":777}}]}}}"#;

#[tokio::test]
async fn login_is_the_first_frame_and_is_verified() {
    let mut srv = server().await;
    let (session, _events) = connect(&srv).await;
    let (n, seen) = next_seen(&mut srv).await;
    assert_eq!(n, 0);
    match seen {
        Seen::Login {
            signer: recovered,
            timestamp_ms,
            nonce,
        } => {
            assert_eq!(recovered, signer().address());
            assert_eq!(nonce.len(), 16);
            let now = std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .unwrap()
                .as_millis() as u64;
            assert!(
                now.abs_diff(timestamp_ms) < 30_000,
                "within the server's plus or minus 30 second window"
            );
        }
        other => panic!("{other:?}"),
    }
    assert_eq!(session.user(), master());
    assert_eq!(session.signer(), signer().address());
    assert!(session.is_connected());
}

#[tokio::test]
async fn login_failures_are_reported() {
    let rejecting = ws_server(WsServerOptions {
        login_host: HOST.into(),
        user: master(),
        reject: Some("bad host".into()),
    })
    .await;
    let err = BinWsSession::connect(BinWsConfig::new(rejecting.url.clone(), HOST, signer()))
        .await
        .err()
        .unwrap();
    assert!(
        matches!(err, Error::Login(ref m) if m.contains("bad host")),
        "{err:?}"
    );

    // The server verifies against another host, so it recovers a different signer.
    let srv = server().await;
    let err = BinWsSession::connect(BinWsConfig::new(
        srv.url.clone(),
        "other-host.example",
        signer(),
    ))
    .await
    .err()
    .unwrap();
    assert!(
        matches!(err, Error::Login(ref m) if m.contains("recovered signer")),
        "{err:?}"
    );

    let err = BinWsSession::connect(BinWsConfig::new("ws://127.0.0.1:1/bin-ws", HOST, signer()))
        .await
        .err()
        .unwrap();
    assert!(matches!(err, Error::Transport(_)), "{err:?}");
}

#[tokio::test]
async fn subscribe_unsubscribe_and_ping_frames() {
    let mut srv = server().await;
    let (session, _events) = connect(&srv).await;
    let _ = next_seen(&mut srv).await;

    session.subscribe(&Subscription::l2("BTC")).await.unwrap();
    session.ping().await.unwrap();
    session.unsubscribe(&Subscription::l2("BTC")).await.unwrap();
    let (_, f) = next_frame(&mut srv).await;
    assert_eq!(
        f.body,
        ClientBody::SubscribeJson(br#"{"type":"l2","c":"BTC"}"#.to_vec())
    );
    assert_eq!(f.id, 0);
    let (_, f) = next_seen_any(&mut srv).await;
    assert_eq!(f.body, ClientBody::Ping);
    let (_, f) = next_frame(&mut srv).await;
    assert_eq!(
        f.body,
        ClientBody::UnsubscribeJson(br#"{"type":"l2","c":"BTC"}"#.to_vec())
    );
}

/// Like `next_frame` but also returns pings.
async fn next_seen_any(srv: &mut WsServer) -> (usize, riverbit_mm_sdk::proto::ClientFrame) {
    loop {
        if let (n, Seen::Frame(f)) = next_seen(srv).await {
            return (n, f);
        }
    }
}

#[tokio::test]
async fn the_session_pings_on_its_own() {
    let mut srv = server().await;
    let mut cfg = config(&srv);
    cfg.ping_interval = Duration::from_millis(40);
    let (_session, _events) = BinWsSession::connect(cfg).await.unwrap();
    let _ = next_seen(&mut srv).await;
    let (_, f) = next_seen_any(&mut srv).await;
    assert_eq!(f.body, ClientBody::Ping);
}

#[tokio::test]
async fn typed_frames_arrive_and_the_book_replays_them() {
    let mut srv = server().await;
    let (_session, mut events) = connect(&srv).await;
    let conn = srv.conns.recv().await.unwrap();

    let snapshot = L2BookFrame {
        coin: "BTC".into(),
        time: 1,
        px_decimals: 1,
        sz_decimals: 3,
        bid_px: vec![1000, 990],
        bid_sz: vec![1000, 2000],
        bid_n: vec![1, 2],
        ask_px: vec![1010, 1020],
        ask_sz: vec![500, 600],
        ask_n: vec![1, 1],
    };
    let update = L2DeltaUpdate {
        coin: "BTC".into(),
        time: 2,
        px_decimals: 1,
        sz_decimals: 3,
        bid_px: vec![1005],
        bid_sz: vec![7],
        bid_removed: vec![0],
        ask_removed: vec![0],
        ..Default::default()
    };
    let bbo = BboFrame {
        coin: "BTC".into(),
        time: 3,
        px_decimals: 1,
        sz_decimals: 3,
        bid_px: Some(1005),
        bid_sz: Some(7),
        ..Default::default()
    };
    let candle = CandleFrame {
        coin: "BTC".into(),
        interval: "1m".into(),
        open: 1,
        close: 2,
        high: 3,
        low: 1,
        ..Default::default()
    };
    let trades = TradesFrame {
        coin: "BTC".into(),
        px_decimals: 1,
        sz_decimals: 3,
        trades: vec![Trade {
            buy: true,
            px: 1005,
            sz: 10,
            time: 4,
            tid: 9,
            hash: vec![1; 32],
            buyer: vec![2; 20],
            seller: vec![3; 20],
        }],
    };
    for body in [
        ServerBody::L2Delta(L2DeltaFrame::Snapshot(snapshot.clone())),
        ServerBody::L2Delta(L2DeltaFrame::Update(update)),
        ServerBody::Bbo(bbo.clone()),
        ServerBody::Candle(candle.clone()),
        ServerBody::Trades(trades.clone()),
        ServerBody::L2Book(snapshot),
        ServerBody::Json(
            br#"{"channel":"subscriptionResponse","data":{"method":"subscribe"}}"#.to_vec(),
        ),
    ] {
        conn.push(ServerFrame { id: 0, body });
    }

    let mut book = Book::new("BTC");
    let mut seen_kinds = Vec::new();
    for _ in 0..7 {
        let Event::Frame(frame) = next_event(&mut events).await else {
            panic!("expected a frame")
        };
        let applied = book.apply_frame(&frame).unwrap();
        seen_kinds.push(match &frame {
            Frame::L2Delta(_) => "l2",
            Frame::Bbo(b) => {
                assert_eq!(b, &bbo);
                "bbo"
            }
            Frame::Candle(c) => {
                assert_eq!(c, &candle);
                "candle"
            }
            Frame::Trades(t) => {
                assert_eq!(t, &trades);
                "trades"
            }
            Frame::L2Book(_) => "l2Book",
            Frame::Json(j) => {
                assert_eq!(j.channel(), Some("subscriptionResponse"));
                "json"
            }
        });
        assert_eq!(applied, matches!(frame, Frame::L2Delta(_)));
    }
    assert_eq!(
        seen_kinds,
        ["l2", "l2", "bbo", "candle", "trades", "l2Book", "json"]
    );
    assert_eq!(book.best_bid().unwrap().px, "100.5");
    assert_eq!(book.best_ask().unwrap().px, "102.0");
    assert_eq!(book.bids().len(), 2);
}

#[tokio::test]
async fn typed_streams_that_fall_back_to_json_still_drive_the_book() {
    let mut srv = server().await;
    let (_session, mut events) = connect(&srv).await;
    let conn = srv.conns.recv().await.unwrap();
    conn.json(0, r#"{"channel":"l2","data":{"s":{"coin":"BTC","time":1,"levels":[[{"px":"100.0","sz":"1.0","n":1}],[{"px":"101.0","sz":"2.0","n":1}]]}}}"#);
    conn.json(0, r#"{"channel":"l2","data":{"u":{"c":"BTC","t":2,"l":[[{"p":"100.5","s":"3.0"}],[]],"r":[[],[0]]}}}"#);
    conn.json(
        0,
        r#"{"channel":"bbo","data":{"coin":"BTC","time":3,"bbo":[null,null]}}"#,
    );
    let mut book = Book::new("BTC");
    let mut applied = Vec::new();
    for _ in 0..3 {
        let Event::Frame(frame) = next_event(&mut events).await else {
            panic!()
        };
        assert!(matches!(frame, Frame::Json(_)));
        applied.push(book.apply_frame(&frame).unwrap());
    }
    assert_eq!(applied, [true, true, false]);
    assert_eq!(book.best_bid().unwrap().px, "100.5");
    assert!(book.best_ask().is_none());
}

#[tokio::test]
async fn replies_are_matched_to_requests_by_id() {
    let mut srv = server().await;
    let (session, _events) = connect(&srv).await;
    let conn = srv.conns.recv().await.unwrap();

    let s1 = session.clone();
    let s2 = session.clone();
    let first =
        tokio::spawn(async move { s1.info(&json!({"type": "l2Book", "coin": "BTC"})).await });
    let (_, a) = next_frame(&mut srv).await;
    let second =
        tokio::spawn(async move { s2.info(&json!({"type": "l2Book", "coin": "ETH"})).await });
    let (_, b) = next_frame(&mut srv).await;
    assert!(a.id != 0 && b.id != 0 && a.id != b.id, "unique nonzero ids");
    let ClientBody::InfoJson(body_a) = &a.body else {
        panic!()
    };
    assert_eq!(body_a, br#"{"type":"l2Book","coin":"BTC"}"#);

    // Answer out of order.
    conn.json(
        b.id,
        &post_reply(b.id, "info", r#"{"type":"l2Book","data":{"coin":"ETH"}}"#),
    );
    conn.json(
        a.id,
        &post_reply(a.id, "info", r#"{"type":"l2Book","data":{"coin":"BTC"}}"#),
    );
    assert_eq!(first.await.unwrap().unwrap()["coin"], "BTC");
    assert_eq!(second.await.unwrap().unwrap()["coin"], "ETH");
}

#[tokio::test]
async fn actions_go_out_as_the_signed_bytes_and_replies_parse_to_receipts() {
    let mut srv = server().await;
    let (session, _events) = connect(&srv).await;
    let conn = srv.conns.recv().await.unwrap();
    let ex = ExchangeClient::with_transport(
        Arc::new(session.clone()),
        signer(),
        SigningDomain::new(12589),
    );
    let request = ex.prepare(&place_one()).unwrap();

    let sending = {
        let request = request.clone();
        let ex = Arc::new(ex);
        tokio::spawn(async move { ex.submit(&request).await })
    };
    let (_, frame) = next_frame(&mut srv).await;
    let ClientBody::ActionJson(bytes) = &frame.body else {
        panic!("{frame:?}")
    };
    assert_eq!(
        bytes.as_slice(),
        request.body(),
        "action_json carries the signed bytes unchanged"
    );
    assert_ne!(frame.id, 0);
    conn.json(frame.id, &post_reply(frame.id, "action", RESTING));
    let receipt = sending.await.unwrap().unwrap();
    assert_eq!(receipt.oids(), vec![Some(777)]);
    assert_eq!(receipt.nonce, request.nonce());
}

#[tokio::test]
async fn an_error_reply_to_an_action_is_an_unknown_outcome() {
    let mut srv = server().await;
    let (session, _events) = connect(&srv).await;
    let conn = srv.conns.recv().await.unwrap();
    let ex = Arc::new(ExchangeClient::with_transport(
        Arc::new(session),
        signer(),
        SigningDomain::new(12589),
    ));
    let sending = {
        let ex = ex.clone();
        tokio::spawn(async move { ex.send(&place_one()).await })
    };
    let (_, frame) = next_frame(&mut srv).await;
    conn.json(
        frame.id,
        &post_reply(frame.id, "error", r#""429 Too Many Requests""#),
    );
    let err = sending.await.unwrap().unwrap_err();
    let unknown = err.as_unknown_outcome().unwrap();
    assert!(
        matches!(unknown.reason, UnknownReason::RateLimited { .. }),
        "{:?}",
        unknown.reason
    );
}

#[tokio::test]
async fn a_disconnect_during_an_action_is_an_unknown_outcome_with_the_request() {
    let mut srv = server().await;
    let (session, mut events) = connect(&srv).await;
    let conn = srv.conns.recv().await.unwrap();
    let ex = Arc::new(ExchangeClient::with_transport(
        Arc::new(session.clone()),
        signer(),
        SigningDomain::new(12589),
    ));
    let request = ex.prepare(&place_one()).unwrap();
    let sending = {
        let ex = ex.clone();
        let request = request.clone();
        tokio::spawn(async move { ex.submit(&request).await })
    };
    let (_, frame) = next_frame(&mut srv).await;
    assert!(matches!(frame.body, ClientBody::ActionJson(_)));
    conn.drop_connection();

    let err = sending.await.unwrap().unwrap_err();
    let unknown = err.as_unknown_outcome().expect("unknown outcome");
    assert!(
        matches!(unknown.reason, UnknownReason::Disconnected(_)),
        "{:?}",
        unknown.reason
    );
    assert_eq!(unknown.nonce(), request.nonce());
    assert_eq!(unknown.request.body(), request.body());
    assert!(matches!(
        next_event(&mut events).await,
        Event::Disconnected { .. }
    ));
}

#[tokio::test]
async fn reconnect_logs_in_again_and_resubscribes() {
    let mut srv = server().await;
    let (session, mut events) = connect(&srv).await;
    let conn = srv.conns.recv().await.unwrap();
    let (_, first_login) = next_seen(&mut srv).await;
    let Seen::Login {
        nonce: first_nonce, ..
    } = first_login
    else {
        panic!()
    };

    session.subscribe(&Subscription::l2("BTC")).await.unwrap();
    session.subscribe(&Subscription::bbo("ETH")).await.unwrap();
    let _ = next_frame(&mut srv).await;
    let _ = next_frame(&mut srv).await;

    conn.drop_connection();
    assert!(matches!(
        next_event(&mut events).await,
        Event::Disconnected { .. }
    ));

    // While down, a subscription is remembered for the next login.
    let late = session.subscribe(&Subscription::trades("SOL")).await;
    let reconnected = loop {
        match next_event(&mut events).await {
            Event::Reconnected {
                attempts,
                user,
                signer: s,
            } => break (attempts, user, s),
            Event::Disconnected { .. } => {}
            other => panic!("{other:?}"),
        }
    };
    assert!(reconnected.0 >= 1);
    assert_eq!(reconnected.1, master());
    assert_eq!(reconnected.2, signer().address());
    assert!(late.is_ok());
    assert!(session.is_connected());

    // New connection: a fresh login (new nonce), then every subscription again.
    let mut subs = Vec::new();
    let mut second_nonce = None;
    while subs.len() < 3 {
        let (n, seen) = next_seen(&mut srv).await;
        match seen {
            Seen::Login { nonce, .. } if n == 1 => second_nonce = Some(nonce),
            Seen::Frame(f) if n == 1 => {
                if let ClientBody::SubscribeJson(b) = f.body {
                    subs.push(String::from_utf8(b).unwrap());
                }
            }
            _ => {}
        }
    }
    assert_ne!(
        second_nonce.unwrap(),
        first_nonce,
        "a login nonce is never reused"
    );
    subs.sort();
    assert_eq!(
        subs,
        [
            r#"{"type":"bbo","coin":"ETH"}"#,
            r#"{"type":"l2","c":"BTC"}"#,
            r#"{"type":"trades","coin":"SOL"}"#
        ]
    );

    // The new connection works for requests.
    let conn2 = srv.conns.recv().await.unwrap();
    let s2 = session.clone();
    let query =
        tokio::spawn(async move { s2.info(&json!({"type": "l2Book", "coin": "BTC"})).await });
    let (_, f) = next_frame(&mut srv).await;
    conn2.json(
        f.id,
        &post_reply(f.id, "info", r#"{"type":"l2Book","data":{"ok":true}}"#),
    );
    assert_eq!(query.await.unwrap().unwrap()["ok"], true);
}

#[tokio::test]
async fn without_reconnect_the_session_ends() {
    let mut srv = server().await;
    let mut cfg = config(&srv);
    cfg.reconnect = None;
    let (session, mut events) = BinWsSession::connect(cfg).await.unwrap();
    let conn = srv.conns.recv().await.unwrap();
    conn.drop_connection();
    assert!(matches!(
        next_event(&mut events).await,
        Event::Disconnected { .. }
    ));
    assert!(matches!(
        next_event(&mut events).await,
        Event::Closed { .. }
    ));
    assert!(events.next().await.is_none());
    assert!(matches!(session.ping().await, Err(Error::Closed(_))));
}

#[tokio::test]
async fn silence_from_the_server_is_a_lost_connection() {
    let mut srv = server().await;
    let mut cfg = config(&srv);
    cfg.reconnect = None;
    cfg.idle_timeout = Duration::from_millis(300);
    cfg.ping_interval = Duration::from_secs(60);
    let (_session, mut events) = BinWsSession::connect(cfg).await.unwrap();
    let _conn = srv.conns.recv().await.unwrap();
    match next_event(&mut events).await {
        Event::Disconnected { reason } => assert!(reason.contains("no message"), "{reason}"),
        other => panic!("{other:?}"),
    }
}

#[tokio::test]
async fn closing_ends_the_session() {
    let mut srv = server().await;
    let (session, mut events) = connect(&srv).await;
    let _conn = srv.conns.recv().await.unwrap();
    session.close().await;
    assert!(matches!(
        next_event(&mut events).await,
        Event::Closed { .. }
    ));
}

#[tokio::test]
async fn info_queries_use_bin_ws_except_the_rest_only_types() {
    let mut srv = server().await;
    let (session, _events) = connect(&srv).await;
    let conn = srv.conns.recv().await.unwrap();
    let http = http_server(|_, _| HttpReply::ok(r#"{"role":"user"}"#)).await;
    let info = InfoClient::new(&http.base_url)
        .unwrap()
        .with_bin_ws(Arc::new(session));

    let query = {
        let info = info.clone();
        tokio::spawn(async move { info.l2_book("BTC").await })
    };
    let (_, f) = next_frame(&mut srv).await;
    conn.json(
        f.id,
        &post_reply(
            f.id,
            "info",
            r#"{"type":"l2Book","data":{"coin":"BTC","time":1,"levels":[[],[]]}}"#,
        ),
    );
    assert_eq!(query.await.unwrap().unwrap().coin, "BTC");
    assert!(
        http.requests.lock().unwrap().is_empty(),
        "l2Book went over bin-ws"
    );

    // userRole must use REST.
    let role = info.user_role(&signer().address()).await.unwrap();
    assert_eq!(role, riverbit_mm_sdk::info::UserRole::User);
    assert_eq!(http.requests.lock().unwrap().len(), 1);
}

#[tokio::test]
async fn posting_while_disconnected_fails_fast() {
    let mut srv = server().await;
    let mut cfg = config(&srv);
    cfg.reconnect = Some(ReconnectPolicy {
        initial_backoff: Duration::from_secs(30),
        max_backoff: Duration::from_secs(30),
        max_attempts: None,
    });
    let (session, mut events) = BinWsSession::connect(cfg).await.unwrap();
    let conn = srv.conns.recv().await.unwrap();
    conn.drop_connection();
    assert!(matches!(
        next_event(&mut events).await,
        Event::Disconnected { .. }
    ));
    let ex = ExchangeClient::with_transport(
        Arc::new(session.clone()),
        signer(),
        SigningDomain::new(12589),
    );
    let err = ex.send(&place_one()).await.unwrap_err();
    assert!(
        matches!(
            err.as_unknown_outcome().unwrap().reason,
            UnknownReason::SendFailed(_)
        ),
        "{err:?}"
    );
    session.close().await;
}
