//! Trades over one bin-ws session and reconciles after a reconnect.
//!
//! ```text
//! RIVERBIT_PRIVATE_KEY=0x... RIVERBIT_PRICE=50000 RIVERBIT_SIZE=0.001 \
//!   cargo run --example reconcile_after_reconnect
//! ```
//!
//! The action is sent as `action_json` on the logged-in connection. If the
//! connection drops before the reply arrives the outcome is unknown: the
//! example keeps the request, waits for the session to log in and resubscribe,
//! then looks the order up by its cloid. It never signs the order again.

mod common;

use std::sync::Arc;

use riverbit_mm_sdk::{
    reconcile, BinWsSession, Cloid, Error, Event, ExchangeClient, InfoClient, Order, OrderReport,
    Place, Subscription, Tif, UnknownOutcome,
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
    let exchange =
        ExchangeClient::with_transport(Arc::new(session.clone()), env.signer.clone(), domain);

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

    // Reconcile whatever is pending each time the session comes back.
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
                let mut still_unknown = Vec::new();
                for unknown in pending.drain(..) {
                    let reports: Vec<OrderReport> = reconcile(&info, &master, &unknown)
                        .await
                        .expect("reconcile");
                    for report in &reports {
                        println!(
                            "nonce {}: {:?} -> {:?}",
                            unknown.nonce(),
                            report.target,
                            report.reply.state
                        );
                    }
                    // `unknownOid` is not proof that nothing executed: only a
                    // found order (or enough other evidence) settles a request.
                    if reports.iter().all(|r| r.is_unknown_oid()) {
                        still_unknown.push(unknown);
                    }
                }
                pending = still_unknown;
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
