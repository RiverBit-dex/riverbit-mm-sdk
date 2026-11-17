//! REST clients against an in-process HTTP server.

mod common;

use std::time::{Duration, Instant};

use common::{http_server, HttpReply, TEST_KEY};
use riverbit_mm_sdk::actions::{
    BatchModify, Cancel, CancelByCloid, CancelByCloidItem, CancelItem, ModifyItem, Order, OrderRef,
    Place, Tif,
};
use riverbit_mm_sdk::error::UnknownReason;
use riverbit_mm_sdk::info::{CandleRequest, OrderState, UserRole};
use riverbit_mm_sdk::reconcile::{reconcile, reconcile_orders};
use riverbit_mm_sdk::signing::{Address, SigningDomain};
use riverbit_mm_sdk::{Cloid, Error, ExchangeClient, InfoClient, Signer};
use serde_json::{json, Value};

fn signer() -> Signer {
    Signer::from_hex(TEST_KEY).unwrap()
}

fn user() -> Address {
    "0x2222222222222222222222222222222222222222"
        .parse()
        .unwrap()
}

fn exchange(base: &str) -> ExchangeClient {
    ExchangeClient::new(base, signer(), SigningDomain::new(12589)).unwrap()
}

fn place_one() -> Place {
    Place::new(vec![
        Order::limit(1, true, "80000", "0.001", Tif::Alo).with_cloid(Cloid::from_u128(1))
    ])
}

const RESTING: &str =
    r#"{"status":"ok","response":{"type":"order","data":{"statuses":[{"resting":{"oid":777}}]}}}"#;

#[tokio::test]
async fn info_queries_post_json_and_parse_replies() {
    let server = http_server(|_, req| {
        let q: Value = serde_json::from_slice(&req.body).unwrap();
        assert_eq!(req.path, "/info");
        HttpReply::ok(&match q["type"].as_str().unwrap() {
            "chainIdentity" => r#"{"nextAgentChainId":12589,"chainId":12589,"height":5}"#.to_string(),
            "userRole" => r#"{"role":"agent","data":{"user":"0x2222222222222222222222222222222222222222"}}"#.to_string(),
            "metaAndAssetCtxs" => r#"[{"universe":[{"name":"RBT","szDecimals":2},{"name":"BTC","szDecimals":5}]},[{"markPx":"1"},{"markPx":"2"}]]"#.to_string(),
            "l2Book" => r#"{"coin":"BTC","time":1,"levels":[[{"px":"100.0","sz":"1.0","n":1}],[{"px":"101.0","sz":"2.0","n":2}]]}"#.to_string(),
            "openOrders" => r#"[{"coin":"BTC","side":"B","limitPx":"100.0","sz":"0.5","oid":9,"timestamp":3}]"#.to_string(),
            "frontendOpenOrders" => r#"[{"coin":"BTC","side":"A","limitPx":"101.0","sz":"0.5","oid":10,"timestamp":3,"cloid":"0x00000000000000000000000000000001","origSz":"1.0","tif":"Alo"}]"#.to_string(),
            "candleSnapshot" => "[]".to_string(),
            "userFills" | "clearinghouseState" | "leadingVaults" => "[]".to_string(),
            other => panic!("unexpected query {other}"),
        })
    })
    .await;
    let info = InfoClient::new(&server.base_url).unwrap();

    assert_eq!(
        info.chain_identity().await.unwrap().next_agent_chain_id,
        12589
    );
    assert_eq!(
        info.user_role(&signer().address()).await.unwrap(),
        UserRole::Agent {
            user: user().to_string()
        }
    );
    let meta = info.meta_and_asset_ctxs().await.unwrap();
    assert_eq!(meta.meta.asset_index("BTC"), Some(1));
    assert_eq!(meta.asset_ctxs.len(), 2);
    let book = info.l2_book("BTC").await.unwrap();
    assert_eq!(book.levels[0][0].px, "100.0");
    assert_eq!(book.levels[1][0].n, 2);
    assert_eq!(info.open_orders(&user()).await.unwrap()[0].oid, 9);
    let front = info.frontend_open_orders(&user()).await.unwrap();
    assert_eq!(
        front[0].cloid.as_deref(),
        Some("0x00000000000000000000000000000001")
    );
    assert_eq!(front[0].tif.as_deref(), Some("Alo"));
    info.candle_snapshot(&CandleRequest {
        coin: "BTC".into(),
        interval: "1m".into(),
        start_time: 1,
        end_time: 2,
    })
    .await
    .unwrap();
    info.user_fills(&user()).await.unwrap();
    info.clearinghouse_state(&user()).await.unwrap();
    info.leading_vaults(&user()).await.unwrap();

    // The queries went out with the documented bodies.
    let bodies: Vec<Value> = server
        .requests
        .lock()
        .unwrap()
        .iter()
        .map(|r| serde_json::from_slice(&r.body).unwrap())
        .collect();
    assert_eq!(bodies[0], json!({"type": "chainIdentity"}));
    assert_eq!(
        bodies[1],
        json!({"type": "userRole", "user": signer().address().to_string()})
    );
    assert_eq!(
        bodies[6],
        json!({"type": "candleSnapshot", "req": {"coin": "BTC", "interval": "1m", "startTime": 1, "endTime": 2}})
    );
}

#[tokio::test]
async fn a_429_is_retried_after_retry_after() {
    let server = http_server(|i, _| {
        if i == 0 {
            HttpReply::status(429, "slow down").with_header("retry-after", "1")
        } else {
            HttpReply::ok(r#"{"nextAgentChainId":12589}"#)
        }
    })
    .await;
    let info = InfoClient::new(&server.base_url).unwrap();
    let started = Instant::now();
    assert_eq!(
        info.chain_identity().await.unwrap().next_agent_chain_id,
        12589
    );
    assert!(
        started.elapsed() >= Duration::from_millis(950),
        "waited for Retry-After"
    );
    assert_eq!(server.requests.lock().unwrap().len(), 2);
}

#[tokio::test]
async fn rate_limit_is_reported_when_retries_run_out() {
    let server =
        http_server(|_, _| HttpReply::status(429, "no").with_header("retry-after", "0")).await;
    let info = InfoClient::new(&server.base_url)
        .unwrap()
        .with_max_retries(2);
    let err = info.chain_identity().await.unwrap_err();
    assert!(
        matches!(err, Error::RateLimited { retry_after: Some(d) } if d == Duration::ZERO),
        "{err:?}"
    );
    assert_eq!(server.requests.lock().unwrap().len(), 3);

    // A very long Retry-After is not slept through; the caller decides.
    let server =
        http_server(|_, _| HttpReply::status(429, "no").with_header("retry-after", "3600")).await;
    let info = InfoClient::new(&server.base_url).unwrap();
    let err = info.chain_identity().await.unwrap_err();
    assert!(
        matches!(err, Error::RateLimited { retry_after: Some(d) } if d == Duration::from_secs(3600)),
        "{err:?}"
    );
    assert_eq!(server.requests.lock().unwrap().len(), 1);
}

#[tokio::test]
async fn info_http_errors_are_reported() {
    let server = http_server(|_, _| HttpReply::status(400, "bad request")).await;
    let info = InfoClient::new(&server.base_url).unwrap();
    let err = info.l2_book("NOPE").await.unwrap_err();
    assert!(
        matches!(err, Error::Http { status: 400, ref body } if body == "bad request"),
        "{err:?}"
    );
}

#[tokio::test]
async fn exchange_sends_the_signed_bytes_unchanged() {
    let server = http_server(|_, _| HttpReply::ok(RESTING)).await;
    let ex = exchange(&server.base_url);
    let request = ex.prepare(&place_one()).unwrap();
    let receipt = ex.submit(&request).await.unwrap();
    assert_eq!(receipt.nonce, request.nonce());
    assert_eq!(receipt.oids(), vec![Some(777)]);
    assert!(receipt.all_ok());
    let seen = server.requests.lock().unwrap();
    assert_eq!(seen[0].path, "/exchange");
    assert_eq!(
        seen[0].body,
        request.body(),
        "bytes sent are the bytes signed"
    );
    let sent: Value = serde_json::from_slice(&seen[0].body).unwrap();
    assert_eq!(
        sent["action"]["orders"][0]["p"], "80000",
        "price stays a decimal string"
    );
    assert!(sent.get("vaultAddress").is_none());
}

#[tokio::test]
async fn vault_requests_carry_the_vault_address() {
    let server = http_server(|_, _| HttpReply::ok(RESTING)).await;
    let vault: Address = "0x1111111111111111111111111111111111111111"
        .parse()
        .unwrap();
    let ex = exchange(&server.base_url).for_vault(vault);
    ex.send(&place_one()).await.unwrap();
    let sent: Value = serde_json::from_slice(&server.requests.lock().unwrap()[0].body).unwrap();
    assert_eq!(
        sent["vaultAddress"],
        "0x1111111111111111111111111111111111111111"
    );
}

#[tokio::test]
async fn every_action_kind_is_sent_with_its_fixed_shape() {
    let server = http_server(|_, req| {
        let a: Value = serde_json::from_slice(&req.body).unwrap();
        let legs = match a["action"]["type"].as_str().unwrap() {
            "order" => r#"[{"resting":{"oid":1}}]"#,
            "cancel" | "cancelByCloid" => r#"["success"]"#,
            "batchModify" => r#"[{"resting":{"oid":2}}]"#,
            other => panic!("{other}"),
        };
        HttpReply::ok(&format!(
            r#"{{"status":"ok","response":{{"type":"x","data":{{"statuses":{legs}}}}}}}"#
        ))
    })
    .await;
    let ex = exchange(&server.base_url);
    let order = Order::limit(1, true, "80100", "0.001", Tif::Alo).with_cloid(Cloid::from_u128(1));
    ex.send(&place_one()).await.unwrap();
    ex.send(&Cancel::new(vec![CancelItem { asset: 1, oid: 1 }]))
        .await
        .unwrap();
    ex.send(&CancelByCloid::new(vec![CancelByCloidItem {
        asset: 1,
        cloid: Cloid::from_u128(1),
    }]))
    .await
    .unwrap();
    let receipt = ex
        .send(&BatchModify::new(vec![ModifyItem { oid: 1, order }]))
        .await
        .unwrap();
    assert_eq!(
        receipt.oids(),
        vec![Some(2)],
        "batchModify reports the new oid"
    );
    let kinds: Vec<String> = server
        .requests
        .lock()
        .unwrap()
        .iter()
        .map(|r| {
            serde_json::from_slice::<Value>(&r.body).unwrap()["action"]["type"]
                .as_str()
                .unwrap()
                .to_string()
        })
        .collect();
    assert_eq!(kinds, ["order", "cancel", "cancelByCloid", "batchModify"]);
}

#[tokio::test]
async fn explicit_rejection_is_final() {
    let server = http_server(|_, _| {
        HttpReply::ok(r#"{"status":"err","response":"action rejected by execution (code 1160): Guild orders must use Alo"}"#)
    })
    .await;
    let err = exchange(&server.base_url)
        .send(&place_one())
        .await
        .unwrap_err();
    let rejected = err.as_rejected().expect("rejected");
    assert_eq!(rejected.code, Some(1160));
}

#[tokio::test]
async fn per_leg_rejection_is_in_the_receipt_not_an_error() {
    let server = http_server(|_, _| {
        HttpReply::ok(r#"{"status":"ok","response":{"type":"order","data":{"statuses":[{"error":"Order must have minimum value of $10."}]}}}"#)
    })
    .await;
    let receipt = exchange(&server.base_url).send(&place_one()).await.unwrap();
    assert!(!receipt.all_ok());
    assert_eq!(receipt.rejected_legs()[0].0, 0);
}

#[tokio::test]
async fn failures_on_a_write_are_unknown_outcomes_with_the_signed_bytes() {
    // Each case: an HTTP answer that does not prove a final outcome.
    let cases: Vec<(HttpReply, UnknownReason)> = vec![
        (
            HttpReply::status(500, "boom"),
            UnknownReason::HttpStatus(500),
        ),
        (
            HttpReply::status(503, "busy"),
            UnknownReason::HttpStatus(503),
        ),
        (
            HttpReply::ok("<html>not json</html>"),
            UnknownReason::MalformedReply,
        ),
        (
            HttpReply::status(429, "slow").with_header("retry-after", "7"),
            UnknownReason::RateLimited {
                retry_after: Some(Duration::from_secs(7)),
            },
        ),
        (
            HttpReply::ok(r#"{"status":"err","response":"action already pending"}"#),
            UnknownReason::AmbiguousReceipt,
        ),
    ];
    for (reply, reason) in cases {
        let (status, body, headers) = (reply.status, reply.body.clone(), reply.headers.clone());
        let server = http_server(move |_, _| HttpReply {
            status,
            body: body.clone(),
            headers: headers.clone(),
        })
        .await;
        let ex = exchange(&server.base_url);
        let request = ex.prepare(&place_one()).unwrap();
        let err = ex.submit(&request).await.unwrap_err();
        let unknown = err
            .as_unknown_outcome()
            .unwrap_or_else(|| panic!("{err:?}"));
        assert_eq!(unknown.reason, reason);
        assert_eq!(unknown.nonce(), request.nonce());
        assert_eq!(
            unknown.request.body(),
            request.body(),
            "carries the exact signed bytes"
        );
        assert_eq!(
            unknown.request.targets(),
            [OrderRef::Cloid(Cloid::from_u128(1))]
        );
        // Resubmitting resends the same bytes, never a new nonce.
        let _ = ex.submit(&unknown.request).await;
        let seen = server.requests.lock().unwrap();
        assert_eq!(seen.len(), 2);
        assert_eq!(seen[0].body, seen[1].body);
    }
}

#[tokio::test]
async fn a_silent_server_is_an_unknown_outcome() {
    // Accepts the connection and never answers.
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let base = format!("http://{}", listener.local_addr().unwrap());
    tokio::spawn(async move {
        let mut held = Vec::new();
        while let Ok((s, _)) = listener.accept().await {
            held.push(s);
        }
    });
    let ex = exchange(&base).with_timeout(Duration::from_millis(300));
    let err = ex.send(&place_one()).await.unwrap_err();
    assert_eq!(
        err.as_unknown_outcome().unwrap().reason,
        UnknownReason::Timeout
    );
}

#[tokio::test]
async fn a_dead_endpoint_is_an_unknown_outcome_not_a_rejection() {
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let base = format!("http://{}", listener.local_addr().unwrap());
    drop(listener);
    let err = exchange(&base).send(&place_one()).await.unwrap_err();
    assert!(err.is_unknown_outcome(), "{err:?}");
}

#[tokio::test]
async fn invalid_actions_fail_before_signing() {
    let server = http_server(|_, _| HttpReply::ok(RESTING)).await;
    let ex = exchange(&server.base_url);
    let bad = Place::new(vec![Order::limit(1, true, "1e3", "1", Tif::Alo)]);
    assert!(matches!(ex.send(&bad).await, Err(Error::InvalidInput(_))));
    assert!(server.requests.lock().unwrap().is_empty());
}

#[tokio::test]
async fn nonces_increase_across_clients_of_one_key() {
    let server = http_server(|_, _| HttpReply::ok(RESTING)).await;
    let a = exchange(&server.base_url);
    let b = exchange(&server.base_url);
    let mut last = 0;
    for ex in [&a, &b, &a, &b] {
        let n = ex.prepare(&place_one()).unwrap().nonce();
        assert!(n > last);
        last = n;
    }
}

#[tokio::test]
async fn reconcile_looks_orders_up_by_cloid_and_oid() {
    let server = http_server(|_, req| {
        let q: Value = serde_json::from_slice(&req.body).unwrap();
        assert_eq!(q["type"], "orderStatus");
        match &q["oid"] {
            Value::String(c) => {
                assert_eq!(c, "0x00000000000000000000000000000001", "cloid is a lowercase string");
                HttpReply::ok(r#"{"status":"order","height":10,"order":{"status":"open","order":{"oid":777,"cloid":"0x00000000000000000000000000000001"}}}"#)
            }
            Value::Number(n) => {
                assert_eq!(n.as_u64(), Some(5), "oid is a JSON number");
                HttpReply::ok(r#"{"status":"unknownOid","height":10}"#)
            }
            other => panic!("{other}"),
        }
    })
    .await;
    let info = InfoClient::new(&server.base_url).unwrap();
    let reports = reconcile_orders(
        &info,
        &user(),
        &[OrderRef::Cloid(Cloid::from_u128(1)), OrderRef::Oid(5)],
    )
    .await
    .unwrap();
    assert!(reports[0].is_open());
    match &reports[0].reply.state {
        OrderState::Order(o) => assert_eq!(o.oid, Some(777)),
        other => panic!("{other:?}"),
    }
    assert!(reports[1].is_unknown_oid() && !reports[1].is_terminal());
}

#[tokio::test]
async fn reconcile_an_unknown_outcome() {
    let exchange_server = http_server(|_, _| HttpReply::status(502, "bad gateway")).await;
    let ex = exchange(&exchange_server.base_url);
    let err = ex.send(&place_one()).await.unwrap_err();
    let unknown = err.as_unknown_outcome().unwrap();

    let info_server = http_server(|_, _| {
        HttpReply::ok(
            r#"{"status":"order","height":11,"order":{"status":"filled","order":{"oid":900}}}"#,
        )
    })
    .await;
    let info = InfoClient::new(&info_server.base_url).unwrap();
    let reports = reconcile(&info, &user(), unknown).await.unwrap();
    assert_eq!(reports.len(), 1);
    assert!(reports[0].is_terminal());

    // A request with no oid or cloid cannot be looked up this way.
    let no_cloid = Place::new(vec![Order::limit(1, true, "1", "1", Tif::Alo)]);
    let err = ex.send(&no_cloid).await.unwrap_err();
    assert!(matches!(
        reconcile(&info, &user(), err.as_unknown_outcome().unwrap()).await,
        Err(Error::InvalidInput(_))
    ));
}
