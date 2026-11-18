//! Request quota: `userRateLimit`, quota rejections, `reserveRequestWeight`
//! purchases and the optional gateway credential. In-process servers only.

mod common;

use common::{http_server, HttpReply, TEST_KEY};
use riverbit_mm_sdk::actions::{Cancel, CancelItem, ReserveRequestWeight};
use riverbit_mm_sdk::error::UnknownReason;
use riverbit_mm_sdk::reconcile::reconcile;
use riverbit_mm_sdk::signing::{Address, SigningDomain};
use riverbit_mm_sdk::{
    Cloid, Error, ExchangeClient, InfoClient, Order, Place, Signer, Tif, UnknownOutcome,
};
use serde_json::Value;

const QUOTA: &str = "Too many cumulative requests sent (10045 > 10025) for cumulative volume traded $25.88. Place taker orders to free up 1 request per USDC traded.";
const SECRET: &str = "mm-credential-0123456789abcdef";

fn signer() -> Signer {
    Signer::from_hex(TEST_KEY).unwrap()
}

fn account() -> Address {
    "0x2222222222222222222222222222222222222222"
        .parse()
        .unwrap()
}

fn vault() -> Address {
    "0x1111111111111111111111111111111111111111"
        .parse()
        .unwrap()
}

fn exchange(base: &str) -> ExchangeClient {
    ExchangeClient::new(base, signer(), SigningDomain::new(12589)).unwrap()
}

#[tokio::test]
async fn user_rate_limit_is_queried_and_remaining_is_computed() {
    let server = http_server(|_, req| {
        let q: Value = serde_json::from_slice(&req.body).unwrap();
        assert_eq!(req.path, "/info");
        assert_eq!(q["type"], "userRateLimit");
        assert_eq!(q["user"], "0x2222222222222222222222222222222222222222");
        HttpReply::ok(
            r#"{"cumVlm":"25.88","nRequestsCap":10025,"nRequestsSurplus":0,"nRequestsUsed":45}"#,
        )
    })
    .await;
    let info = InfoClient::new(&server.base_url).unwrap();
    let limit = info.user_rate_limit(&account()).await.unwrap();
    assert_eq!(limit.cum_vlm, "25.88");
    assert_eq!(limit.n_requests_cap, 10025);
    assert_eq!(limit.remaining(), 9980);
}

#[tokio::test]
async fn an_unsupported_node_is_a_normal_read_error() {
    let server = http_server(|_, _| HttpReply::status(501, "not implemented")).await;
    let info = InfoClient::new(&server.base_url).unwrap();
    let err = info.user_rate_limit(&account()).await.unwrap_err();
    assert!(matches!(err, Error::Http { status: 501, .. }), "{err:?}");
}

#[tokio::test]
async fn a_partly_accepted_order_reports_quota_legs_as_final_rejections() {
    let body = format!(
        r#"{{"status":"ok","response":{{"type":"order","data":{{"statuses":[{{"resting":{{"oid":9}}}},{{"error":"action rejected by execution (code 1800) ({QUOTA})"}}]}}}}}}"#
    );
    let server = http_server(move |_, _| HttpReply::ok(&body)).await;
    let ex = exchange(&server.base_url);
    let place = Place::new(vec![
        Order::limit(1, true, "80000", "0.001", Tif::Alo).with_cloid(Cloid::from_u128(1)),
        Order::limit(1, true, "79999", "0.001", Tif::Alo).with_cloid(Cloid::from_u128(2)),
    ]);
    let receipt = ex.send(&place).await.unwrap();
    assert_eq!(receipt.oids(), vec![Some(9), None]);
    assert_eq!(receipt.quota_rejected_legs(), vec![1]);
    assert!(!receipt.all_ok());
}

#[tokio::test]
async fn a_quota_rejected_cancel_leg_with_an_asset_suffix_is_recognized() {
    let body = format!(
        r#"{{"status":"ok","response":{{"type":"cancel","data":{{"statuses":[{{"error":"{QUOTA} asset=1"}}]}}}}}}"#
    );
    let server = http_server(move |_, _| HttpReply::ok(&body)).await;
    let receipt = exchange(&server.base_url)
        .send(&Cancel::new(vec![CancelItem { asset: 1, oid: 5 }]))
        .await
        .unwrap();
    assert_eq!(receipt.quota_rejected_legs(), vec![0]);
}

#[tokio::test]
async fn a_whole_action_quota_rejection_is_final_not_unknown() {
    let body = format!(
        r#"{{"status":"err","response":"{QUOTA}","committedResult":{{"type":"order","data":{{"statuses":[{{"error":"x"}}]}}}}}}"#
    );
    let server = http_server(move |_, _| HttpReply::ok(&body)).await;
    let order = Order::limit(1, true, "80000", "0.001", Tif::Alo);
    let modify = riverbit_mm_sdk::actions::Modify { oid: 5, order };
    let err = exchange(&server.base_url).send(&modify).await.unwrap_err();
    assert!(err.is_quota_exhausted(), "{err:?}");
    assert!(!err.is_unknown_outcome());
    let rejected = err.as_rejected().unwrap();
    assert!(
        rejected.message.contains("10045 > 10025"),
        "the original text stays available"
    );
}

#[tokio::test]
async fn a_purchase_is_signed_like_an_order_and_acknowledged_without_legs() {
    let server =
        http_server(|_, _| HttpReply::ok(r#"{"status":"ok","response":{"type":"default"}}"#)).await;
    let ex = exchange(&server.base_url);
    let receipt = ex.send(&ReserveRequestWeight::new(1000)).await.unwrap();
    assert!(receipt.default_ack && receipt.legs.is_empty());

    let to_vault = ReserveRequestWeight::new(250).with_destination(vault());
    ex.send(&to_vault).await.unwrap();
    let seen = server.requests.lock().unwrap();
    assert_eq!(seen[0].path, "/exchange");
    let first: Value = serde_json::from_slice(&seen[0].body).unwrap();
    assert_eq!(
        first["action"],
        serde_json::json!({"type":"reserveRequestWeight","weight":1000})
    );
    assert!(first.get("vaultAddress").is_none());
    let second: Value = serde_json::from_slice(&seen[1].body).unwrap();
    assert_eq!(
        second["action"]["destination"],
        "0x1111111111111111111111111111111111111111"
    );
    assert!(second.get("vaultAddress").is_none());
}

#[tokio::test]
async fn an_invalid_purchase_fails_before_signing_or_sending() {
    let server = http_server(|_, _| HttpReply::ok("{}")).await;
    let ex = exchange(&server.base_url);
    for weight in [0, 1_000_000_001] {
        let err = ex.send(&ReserveRequestWeight::new(weight)).await;
        assert!(matches!(err, Err(Error::InvalidInput(_))), "{weight}");
    }
    // A vault client pays and receives: another destination is refused.
    let vault_client = exchange(&server.base_url).for_vault(vault());
    let other = ReserveRequestWeight::new(5).with_destination(account());
    assert!(matches!(
        vault_client.send(&other).await,
        Err(Error::InvalidInput(_))
    ));
    // Absent, or equal to the vault, is fine for a vault client.
    assert!(vault_client.prepare(&ReserveRequestWeight::new(5)).is_ok());
    assert!(vault_client
        .prepare(&ReserveRequestWeight::new(5).with_destination(vault()))
        .is_ok());
    assert!(server.requests.lock().unwrap().is_empty());
}

#[tokio::test]
async fn purchase_rejections_are_final_and_everything_else_is_unknown() {
    let overflow = r#"{"status":"ok","response":{"type":"order","data":{"statuses":[{"error":"action rejected by execution (code 1801) (reserved request weight would overflow)"}]}}}"#;
    let server = http_server(move |_, _| HttpReply::ok(overflow)).await;
    let err = exchange(&server.base_url)
        .send(&ReserveRequestWeight::new(10))
        .await
        .unwrap_err();
    let rejected = err.as_rejected().expect("final rejection");
    assert_eq!(rejected.code, Some(1801));
    assert!(rejected.is_purchase_overflow());

    let server = http_server(|_, _| HttpReply::ok(r#"{"status":"err","response":"nope"}"#)).await;
    let err = exchange(&server.base_url)
        .send(&ReserveRequestWeight::new(10))
        .await
        .unwrap_err();
    assert!(err.as_rejected().is_some());

    for reply in [
        HttpReply::status(502, "bad gateway"),
        HttpReply::ok("not json"),
        HttpReply::ok(r#"{"status":"ok","response":{"type":"order","data":{"statuses":[]}}}"#),
        HttpReply::ok(r#"{"status":"err","response":"action already pending"}"#),
    ] {
        let reply = std::sync::Mutex::new(Some(reply));
        let server = http_server(move |_, _| reply.lock().unwrap().take().unwrap()).await;
        let err = exchange(&server.base_url)
            .send(&ReserveRequestWeight::new(10))
            .await
            .unwrap_err();
        assert!(err.is_unknown_outcome(), "{err:?}");
    }
}

#[tokio::test]
async fn an_unknown_purchase_is_not_reconcilable_by_order_lookup() {
    let server = http_server(|_, _| HttpReply::status(502, "bad gateway")).await;
    let err = exchange(&server.base_url)
        .send(&ReserveRequestWeight::new(10))
        .await
        .unwrap_err();
    let unknown: &UnknownOutcome = err.as_unknown_outcome().unwrap();
    assert_eq!(unknown.reason, UnknownReason::HttpStatus(502));
    assert!(unknown.request.targets().is_empty());
    // The info server must not be asked anything.
    let info_server = http_server(|_, _| panic!("no order lookup for a purchase")).await;
    let info = InfoClient::new(&info_server.base_url).unwrap();
    let result = reconcile(&info, &account(), unknown).await;
    match result {
        Err(Error::InvalidInput(text)) => assert!(text.contains("userRateLimit"), "{text}"),
        other => panic!("{other:?}"),
    }
    assert!(info_server.requests.lock().unwrap().is_empty());
}

#[tokio::test]
async fn the_gateway_credential_is_sent_on_info_and_exchange_only_when_set() {
    let server = http_server(|_, req| {
        if req.path == "/info" {
            HttpReply::ok(r#"{"nextAgentChainId":12589}"#)
        } else {
            HttpReply::ok(r#"{"status":"ok","response":{"type":"default"}}"#)
        }
    })
    .await;
    // Unset and empty: no header.
    let plain = InfoClient::new(&server.base_url).unwrap();
    plain.chain_identity().await.unwrap();
    let empty = InfoClient::new(&server.base_url)
        .unwrap()
        .with_gateway_token("")
        .unwrap();
    empty.chain_identity().await.unwrap();
    exchange(&server.base_url)
        .with_gateway_token("")
        .unwrap()
        .send(&ReserveRequestWeight::new(1))
        .await
        .unwrap();
    // Set: the header is on every request.
    let info = InfoClient::new(&server.base_url)
        .unwrap()
        .with_gateway_token(SECRET)
        .unwrap();
    info.chain_identity().await.unwrap();
    exchange(&server.base_url)
        .with_gateway_token(SECRET)
        .unwrap()
        .send(&ReserveRequestWeight::new(1))
        .await
        .unwrap();
    let seen = server.requests.lock().unwrap();
    assert_eq!(seen.len(), 5);
    for r in &seen[..3] {
        assert_eq!(r.header("x-riverbit-mm-token"), None, "{}", r.path);
    }
    for r in &seen[3..] {
        assert_eq!(r.header("x-riverbit-mm-token"), Some(SECRET), "{}", r.path);
    }
    // The credential is not part of the signed body.
    assert!(!String::from_utf8_lossy(&seen[4].body).contains(SECRET));
}

#[tokio::test]
async fn the_credential_does_not_follow_a_redirect() {
    let target = http_server(|_, _| HttpReply::ok("{}")).await;
    let location = format!("{}/info", target.base_url);
    let origin =
        http_server(move |_, _| HttpReply::status(307, "").with_header("location", &location))
            .await;
    let info = InfoClient::new(&origin.base_url)
        .unwrap()
        .with_gateway_token(SECRET)
        .unwrap();
    assert!(info.chain_identity().await.is_err());
    assert!(target.requests.lock().unwrap().is_empty());
}

#[test]
fn invalid_credentials_are_rejected_at_configuration_time() {
    for bad in [
        "a\r\nX-Evil: 1",
        "a\nb",
        "a\rb",
        "tok\u{e9}n",
        "a b",
        "a\0b",
    ] {
        let info = InfoClient::new("http://127.0.0.1:9").unwrap();
        let err = info.with_gateway_token(bad).unwrap_err();
        assert!(matches!(err, Error::InvalidInput(_)), "{bad:?}");
        assert!(!err.to_string().contains("Evil"), "{err}");
        let ex = exchange("http://127.0.0.1:9");
        assert!(ex.with_gateway_token(bad).is_err(), "{bad:?}");
    }
}

#[tokio::test]
async fn debug_output_never_shows_the_credential() {
    let info = InfoClient::new("http://127.0.0.1:9")
        .unwrap()
        .with_gateway_token(SECRET)
        .unwrap();
    let ex = exchange("http://127.0.0.1:9")
        .with_gateway_token(SECRET)
        .unwrap();
    for shown in [
        format!("{info:?}"),
        format!("{info:#?}"),
        format!("{ex:?}"),
        format!("{ex:#?}"),
    ] {
        assert!(!shown.contains(SECRET), "{shown}");
        assert!(
            !shown.contains(&hex::encode(SECRET)) && !shown.contains("Authorization"),
            "{shown}"
        );
    }
    // Errors and unknown outcomes (which carry the request, not the header)
    // do not show it either.
    let err = ex
        .send(&Place::new(vec![Order::limit(
            1,
            true,
            "80000",
            "0.001",
            Tif::Alo,
        )]))
        .await
        .unwrap_err();
    assert!(!format!("{err} {err:?}").contains(SECRET));
    let err = info.chain_identity().await.unwrap_err();
    assert!(!format!("{err} {err:?}").contains(SECRET));
}
