//! Reproduces every entry of `testdata/vectors.json` byte for byte.

use riverbit_mm_sdk::signing::{
    action_digest, action_msgpack, connection_id, login_message, personal_hash, recover_personal,
    ActionInfo, Address, Signer, SigningDomain,
};
use riverbit_mm_sdk::{
    Action, BatchModify, Cancel, CancelByCloid, CancelByCloidItem, CancelItem, Cloid, Modify,
    ModifyItem, NonceSource, Order, Place, ReserveRequestWeight, Tif,
};
use serde_json::Value;

fn vectors() -> Value {
    let path = concat!(env!("CARGO_MANIFEST_DIR"), "/../testdata/vectors.json");
    serde_json::from_str(&std::fs::read_to_string(path).expect("read vectors.json"))
        .expect("parse vectors.json")
}

fn hex_of(v: &Value) -> Vec<u8> {
    let s = v.as_str().unwrap();
    hex::decode(s.strip_prefix("0x").unwrap_or(s)).unwrap()
}

fn signer(v: &Value) -> Signer {
    let key = &v["test_key"];
    let signer = Signer::from_hex(key["private_key"].as_str().unwrap()).unwrap();
    assert_eq!(
        signer.address().to_string(),
        key["address"].as_str().unwrap().to_lowercase(),
        "address of the test key"
    );
    signer
}

#[test]
fn login_vectors() {
    let v = vectors();
    let signer = signer(&v);
    let logins = v["login"].as_array().unwrap();
    assert!(!logins.is_empty());
    for case in logins {
        let name = case["name"].as_str().unwrap();
        let host = case["host"].as_str().unwrap();
        let ts = case["timestamp_ms"].as_u64().unwrap();
        let nonce: [u8; 16] = hex_of(&case["nonce_hex"]).try_into().unwrap();
        let message = login_message(host, ts, &nonce);
        assert_eq!(
            message,
            case["message"].as_str().unwrap(),
            "{name}: message"
        );
        let login = signer.sign_login(host, ts, nonce).unwrap();
        assert_eq!(login.timestamp_ms, ts, "{name}");
        assert_eq!(login.nonce, nonce, "{name}");
        assert_eq!(
            hex::encode(&login.signature),
            case["signature_hex"].as_str().unwrap(),
            "{name}: signature"
        );
        let recovered = recover_personal(message.as_bytes(), &login.signature).unwrap();
        assert_eq!(recovered, signer.address(), "{name}: recovered signer");
        assert_ne!(personal_hash(message.as_bytes()), [0u8; 32]);
    }
}

#[test]
fn action_vectors() {
    let v = vectors();
    let signer = signer(&v);
    let actions = v["actions"].as_array().unwrap();
    assert!(!actions.is_empty());
    for case in actions {
        let name = case["name"].as_str().unwrap();
        let action_json = case["action_json"].as_str().unwrap();
        let nonce = case["nonce"].as_u64().unwrap();
        let vault: Option<Address> = case["vault_address"].as_str().map(|s| s.parse().unwrap());
        let domain = SigningDomain {
            chain_id: case["chain_id"].as_u64().unwrap(),
            source: case["source"].as_str().unwrap().to_string(),
        };

        let packed = action_msgpack(action_json).unwrap();
        assert_eq!(
            hex::encode(&packed),
            case["msgpack_hex"].as_str().unwrap(),
            "{name}: msgpack"
        );
        let cid = connection_id(&packed, nonce, vault.as_ref());
        assert_eq!(
            cid.to_vec(),
            hex_of(&case["connection_id"]),
            "{name}: connectionId"
        );
        let digest = action_digest(&cid, &domain);
        assert_eq!(digest.to_vec(), hex_of(&case["digest"]), "{name}: digest");

        let sig = signer
            .sign_action(action_json, nonce, vault.as_ref(), &domain)
            .unwrap();
        let want = &case["signature"];
        assert_eq!(sig.r.to_vec(), hex_of(&want["r"]), "{name}: r");
        assert_eq!(sig.s.to_vec(), hex_of(&want["s"]), "{name}: s");
        assert_eq!(u64::from(sig.v), want["v"].as_u64().unwrap(), "{name}: v");

        // The request body embeds the action bytes untouched.
        let info = ActionInfo {
            kind: riverbit_mm_sdk::ActionKind::Order,
            expected_legs: None,
            targets: vec![],
        };
        let req = signer
            .sign_request(
                action_json.to_string(),
                nonce,
                vault.as_ref(),
                &domain,
                info,
            )
            .unwrap();
        let body = std::str::from_utf8(req.body()).unwrap();
        assert!(
            body.starts_with(&format!(
                "{{\"action\":{action_json},\"nonce\":{nonce},\"signature\":{{"
            )),
            "{name}"
        );
        let parsed: Value = serde_json::from_str(body).unwrap();
        assert_eq!(parsed["signature"]["r"], want["r"], "{name}: body r");
        assert_eq!(parsed["signature"]["s"], want["s"], "{name}: body s");
        assert_eq!(parsed["signature"]["v"], want["v"], "{name}: body v");
        match &vault {
            Some(addr) => assert_eq!(
                parsed["vaultAddress"].as_str().unwrap(),
                addr.to_string(),
                "{name}"
            ),
            None => assert!(parsed.get("vaultAddress").is_none(), "{name}"),
        }
    }
}

fn order_of(v: &Value) -> Order {
    let tif = match v["t"]["limit"]["tif"].as_str().unwrap() {
        "Alo" => Tif::Alo,
        "Gtc" => Tif::Gtc,
        other => panic!("tif {other}"),
    };
    let mut order = Order::limit(
        v["a"].as_u64().unwrap() as u32,
        v["b"].as_bool().unwrap(),
        v["p"].as_str().unwrap(),
        v["s"].as_str().unwrap(),
        tif,
    )
    .reduce_only(v["r"].as_bool().unwrap());
    if let Some(c) = v.get("c") {
        order = order.with_cloid(c.as_str().unwrap().parse::<Cloid>().unwrap());
    }
    order
}

/// Builds the typed action that `action_json` describes and returns it
/// boxed with its serialization.
fn typed(action_json: &str) -> Box<dyn Action> {
    let v: Value = serde_json::from_str(action_json).unwrap();
    match v["type"].as_str().unwrap() {
        "order" => Box::new(Place::new(
            v["orders"]
                .as_array()
                .unwrap()
                .iter()
                .map(order_of)
                .collect(),
        )),
        "cancel" => Box::new(Cancel::new(
            v["cancels"]
                .as_array()
                .unwrap()
                .iter()
                .map(|c| CancelItem {
                    asset: c["a"].as_u64().unwrap() as u32,
                    oid: c["o"].as_u64().unwrap(),
                })
                .collect(),
        )),
        "cancelByCloid" => Box::new(CancelByCloid::new(
            v["cancels"]
                .as_array()
                .unwrap()
                .iter()
                .map(|c| CancelByCloidItem {
                    asset: c["asset"].as_u64().unwrap() as u32,
                    cloid: c["cloid"].as_str().unwrap().parse().unwrap(),
                })
                .collect(),
        )),
        "modify" => Box::new(Modify {
            oid: v["oid"].as_u64().unwrap(),
            order: order_of(&v["order"]),
        }),
        "batchModify" => Box::new(BatchModify::new(
            v["modifies"]
                .as_array()
                .unwrap()
                .iter()
                .map(|m| ModifyItem {
                    oid: m["oid"].as_u64().unwrap(),
                    order: order_of(&m["order"]),
                })
                .collect(),
        )),
        "reserveRequestWeight" => {
            let mut action = ReserveRequestWeight::new(v["weight"].as_u64().unwrap());
            if let Some(d) = v.get("destination") {
                action = action.with_destination(d.as_str().unwrap().parse().unwrap());
            }
            Box::new(action)
        }
        other => panic!("unknown action type {other}"),
    }
}

#[test]
fn typed_actions_serialize_to_the_vector_bytes_and_sign_to_the_vector_signature() {
    let v = vectors();
    let signer = signer(&v);
    for case in v["actions"].as_array().unwrap() {
        let name = case["name"].as_str().unwrap();
        let action_json = case["action_json"].as_str().unwrap();
        let action = typed(action_json);
        assert_eq!(
            action.to_json().unwrap(),
            action_json,
            "{name}: typed serialization"
        );

        let vault: Option<Address> = case["vault_address"].as_str().map(|s| s.parse().unwrap());
        let domain = SigningDomain {
            chain_id: case["chain_id"].as_u64().unwrap(),
            source: case["source"].as_str().unwrap().to_string(),
        };
        let info = ActionInfo {
            kind: action.kind(),
            expected_legs: action.expected_legs(),
            targets: action.targets(),
        };
        let nonce = case["nonce"].as_u64().unwrap();
        let request = signer
            .sign_request(
                action.to_json().unwrap(),
                nonce,
                vault.as_ref(),
                &domain,
                info,
            )
            .unwrap();
        let body: Value = serde_json::from_slice(request.body()).unwrap();
        let want = &case["signature"];
        for (field, ours) in [
            ("r", &body["signature"]["r"]),
            ("s", &body["signature"]["s"]),
            ("v", &body["signature"]["v"]),
        ] {
            assert_eq!(ours, &want[field], "{name}: {field}");
        }
        assert_eq!(
            request.kind().as_str(),
            body["action"]["type"].as_str().unwrap()
        );
    }
}

#[test]
fn every_vector_action_type_is_covered_by_a_typed_action() {
    let v = vectors();
    let names: Vec<&str> = v["actions"]
        .as_array()
        .unwrap()
        .iter()
        .map(|c| c["name"].as_str().unwrap())
        .collect();
    for name in [
        "reserve-request-weight-no-vault",
        "reserve-request-weight-destination",
        "reserve-request-weight-vault",
    ] {
        assert!(names.contains(&name), "{name} missing from vectors.json");
    }
}

#[test]
fn nonce_source_is_available() {
    assert!(NonceSource::new().next() > 0);
}
