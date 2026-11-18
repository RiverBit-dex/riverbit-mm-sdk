//! Sends an order over REST, follows order events over bin-ws and reconciles
//! unknown outcomes by cloid.
//!
//! ```text
//! RIVERBIT_KEY_FILE=./key RIVERBIT_PRICE=50000 RIVERBIT_SIZE=0.001 \
//!   cargo run --example reconcile_after_reconnect
//! ```
//!
//! Orders go out as `POST /exchange`. The bin-ws session carries the
//! `orderUpdates` stream.
//! If the order's outcome is unknown (timeout, HTTP error, ambiguous reply) the
//! example keeps the signed request, looks the order up by its cloid, and looks
//! again each time the session reconnects. It never signs the order again.

mod common;

use riverbit_mm_sdk::{
    reconcile, Address, BinWsSession, Cloid, Error, Event, ExchangeClient, InfoClient, Order,
    OrderReport, Place, Subscription, Tif, UnknownOutcome,
};

#[tokio::main]
async fn main() {
    let env = common::Env::load();
    let price = common::required("RIVERBIT_PRICE");
    let size = common::required("RIVERBIT_SIZE");

    let info: InfoClient = env.info();
    let domain = env.domain(&info).await;
    let master = env.master(&info).await;
    let meta = info
        .meta_and_asset_ctxs()
        .await
        .expect("metaAndAssetCtxs")
        .meta;
    let asset = meta.asset_index(&env.coin).expect("unknown coin");

    let (session, mut events) = BinWsSession::connect(env.ws_config())
        .await
        .expect("connect and log in");
    session
        .subscribe(&Subscription::order_updates(&master))
        .await
        .expect("subscribe");
    let exchange = ExchangeClient::new(&env.base_url, env.signer.clone(), domain)
        .expect("build the exchange client");

    let order =
        Order::limit(asset, true, price, size, Tif::Alo).with_cloid(Cloid::random().unwrap());
    let mut pending: Vec<UnknownOutcome> = Vec::new();
    match exchange.send(&Place::new(vec![order])).await {
        Ok(receipt) => println!("receipt: {:?}", receipt.legs),
        Err(Error::UnknownOutcome(unknown)) => {
            println!(
                "unknown outcome ({}); keeping nonce {} pending",
                unknown.reason,
                unknown.nonce()
            );
            pending.push(*unknown);
        }
        Err(e) => eprintln!("{e}"),
    }

    // Look the pending request up now, and again after every reconnect.
    pending = settle(&info, &master, pending).await;

    while let Some(event) = events.next().await {
        match event {
            Event::Frame(frame) => println!("{frame:?}"),
            Event::Disconnected { reason } => println!("disconnected: {reason}"),
            Event::Reconnected { .. } => {
                // The session logged in again and resubscribed. Rebuild state:
                // open orders first, then each pending request.
                let open = info
                    .frontend_open_orders(&master)
                    .await
                    .expect("frontendOpenOrders");
                println!("{} open order(s) after reconnect", open.len());
                pending = settle(&info, &master, std::mem::take(&mut pending)).await;
                if pending.is_empty() {
                    break;
                }
            }
            Event::Closed { reason } => {
                eprintln!("closed: {reason}");
                break;
            }
        }
    }
    session.close().await;
}

/// Reconciles each pending request; returns those that are still unsettled.
async fn settle(
    info: &InfoClient,
    master: &Address,
    pending: Vec<UnknownOutcome>,
) -> Vec<UnknownOutcome> {
    let mut still_unknown = Vec::new();
    for unknown in pending {
        let reports: Vec<OrderReport> = reconcile(info, master, &unknown).await.expect("reconcile");
        for report in &reports {
            println!(
                "nonce {}: {:?} -> {:?}",
                unknown.nonce(),
                report.target,
                report.reply.state
            );
        }
        // `unknownOid` is not proof that nothing executed: only a found order
        // (or enough other evidence) settles a request.
        if reports.iter().all(|r| r.is_unknown_oid()) {
            still_unknown.push(unknown);
        }
    }
    still_unknown
}
