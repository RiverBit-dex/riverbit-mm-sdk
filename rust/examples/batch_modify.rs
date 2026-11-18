//! Places two post-only orders, moves both with one `batchModify` and cancels
//! them.
//!
//! ```text
//! RIVERBIT_KEY_FILE=./key RIVERBIT_SIZE=0.001 \
//!   RIVERBIT_BID=50000 RIVERBIT_ASK=90000 \
//!   RIVERBIT_NEW_BID=50010 RIVERBIT_NEW_ASK=89990 \
//!   cargo run --example batch_modify
//! ```
//!
//! `batchModify` replaces each order with a complete new one; the receipt
//! carries the new oids. Pass the original cloid to keep it.

mod common;

use riverbit_mm_sdk::actions::{BatchModify, CancelByCloid, CancelByCloidItem, ModifyItem};
use riverbit_mm_sdk::{Cloid, ExchangeClient, LegStatus, Order, Place, Tif};

#[tokio::main]
async fn main() {
    let env = common::Env::load();
    let bid = common::required("RIVERBIT_BID");
    let ask = common::required("RIVERBIT_ASK");
    let size = common::required("RIVERBIT_SIZE");
    let new_bid = common::required("RIVERBIT_NEW_BID");
    let new_ask = common::required("RIVERBIT_NEW_ASK");

    let info = env.info();
    let domain = env.domain(&info).await;
    let meta = info
        .meta_and_asset_ctxs()
        .await
        .expect("metaAndAssetCtxs")
        .meta;
    let asset = meta.asset_index(&env.coin).expect("unknown coin");
    let exchange: ExchangeClient = env.exchange(domain);

    let (bid_cloid, ask_cloid) = (Cloid::random().unwrap(), Cloid::random().unwrap());
    let buy = Order::limit(asset, true, bid.clone(), size.clone(), Tif::Alo).with_cloid(bid_cloid);
    let sell =
        Order::limit(asset, false, ask.clone(), size.clone(), Tif::Alo).with_cloid(ask_cloid);
    let receipt = exchange
        .send(&Place::new(vec![buy, sell]))
        .await
        .expect("place");
    // Inspect every leg: one can rest while the other is rejected.
    println!("placed: {:?}", receipt.legs);

    // Replace each resting leg with a complete new order at the new price.
    let mut modifies = Vec::new();
    for (leg, is_buy, cloid, px) in [
        (&receipt.legs[0], true, bid_cloid, &new_bid),
        (&receipt.legs[1], false, ask_cloid, &new_ask),
    ] {
        if let LegStatus::Resting { oid, .. } = leg {
            let order =
                Order::limit(asset, is_buy, px.clone(), size.clone(), Tif::Alo).with_cloid(cloid);
            modifies.push(ModifyItem { oid: *oid, order });
        }
    }
    if modifies.is_empty() {
        println!("nothing rests; stopping");
        return;
    }
    let receipt = exchange
        .send(&BatchModify::new(modifies))
        .await
        .expect("batchModify");
    for (i, leg) in receipt.legs.iter().enumerate() {
        match leg {
            LegStatus::Resting { oid, .. } => println!("modify {i}: new oid {oid}"),
            LegStatus::Error(e) => println!("modify {i}: rejected: {e}"),
            other => println!("modify {i}: {other:?}"),
        }
    }

    let cancel = CancelByCloid::new(vec![
        CancelByCloidItem {
            asset,
            cloid: bid_cloid,
        },
        CancelByCloidItem {
            asset,
            cloid: ask_cloid,
        },
    ]);
    println!("cancel: {:?}", exchange.send(&cancel).await.map(|r| r.legs));
}
