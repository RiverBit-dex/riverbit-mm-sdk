//! Places one post-only (`Alo`) limit order, reads its status back and
//! cancels it.
//!
//! ```text
//! RIVERBIT_KEY_FILE=./key RIVERBIT_PRICE=50000 RIVERBIT_SIZE=0.001 \
//!   cargo run --example place_query_cancel
//! ```
//!
//! Choose a price that rests away from the market and a size that satisfies
//! the market's precision and minimum notional. The example sends real,
//! signed orders to whatever `RIVERBIT_BASE_URL` points at.

mod common;

use riverbit_mm_sdk::actions::{CancelByCloid, CancelByCloidItem};
use riverbit_mm_sdk::{
    reconcile, Cloid, Error, ExchangeClient, LegStatus, Order, OrderRef, Place, Tif,
};

#[tokio::main]
async fn main() {
    let env = common::Env::load();
    let price = common::required("RIVERBIT_PRICE");
    let size = common::required("RIVERBIT_SIZE");

    let info = env.info();
    let domain = env.domain(&info).await;
    let master = env.master(&info).await;
    // Never hard-code the asset index: it is the position in `universe`.
    let meta = info
        .meta_and_asset_ctxs()
        .await
        .expect("metaAndAssetCtxs")
        .meta;
    let asset = meta.asset_index(&env.coin).expect("unknown coin");
    println!(
        "{} is asset {asset}, szDecimals {}",
        env.coin,
        meta.asset(&env.coin).unwrap().sz_decimals
    );

    let exchange = ExchangeClient::new(&env.base_url, env.signer.clone(), domain)
        .expect("build the exchange client");
    let cloid = Cloid::random().expect("cloid");
    let place = Place::new(vec![
        Order::limit(asset, true, price, size, Tif::Alo).with_cloid(cloid)
    ]);

    match exchange.send(&place).await {
        Ok(receipt) => match &receipt.legs[0] {
            LegStatus::Resting { oid, .. } => println!("resting as oid {oid}"),
            LegStatus::Filled {
                oid,
                total_sz,
                avg_px,
                ..
            } => println!("filled {total_sz} @ {avg_px} (oid {oid}); the rest may still rest"),
            LegStatus::Error(e) => {
                eprintln!("leg rejected: {e}");
                return;
            }
            other => println!("{other:?}"),
        },
        Err(Error::Rejected(r)) => {
            eprintln!("rejected, nothing executed: {r}");
            return;
        }
        Err(Error::UnknownOutcome(unknown)) => {
            // Do not sign a replacement. Find out what happened to this one.
            eprintln!(
                "unknown outcome ({}), nonce {}; reconciling",
                unknown.reason,
                unknown.nonce()
            );
            let reports = reconcile(&info, &master, &unknown)
                .await
                .expect("reconcile");
            for r in &reports {
                println!("{:?}: {:?}", r.target, r.reply.state);
            }
            return;
        }
        Err(e) => {
            eprintln!("{e}");
            return;
        }
    }

    let status = info
        .order_status(&master, OrderRef::Cloid(cloid))
        .await
        .expect("orderStatus");
    println!("order status: {:?}", status.state);

    let cancel = CancelByCloid::new(vec![CancelByCloidItem { asset, cloid }]);
    match exchange.send(&cancel).await {
        Ok(receipt) => println!("cancel legs: {:?}", receipt.legs),
        Err(e) => eprintln!("cancel: {e}"),
    }
    let status = info
        .order_status(&master, OrderRef::Cloid(cloid))
        .await
        .expect("orderStatus");
    println!("order status after cancel: {:?}", status.state);
}
