//! Prints the request quota of the account and, only when asked, buys
//! requests.
//!
//! ```text
//! RIVERBIT_KEY_FILE=./key cargo run --example quota
//! RIVERBIT_KEY_FILE=./key BUY_REQUESTS=1000 cargo run --example quota
//! ```
//!
//! Without `BUY_REQUESTS` the example sends only unsigned read-only `/info`
//! queries. With it, the example signs one `reserveRequestWeight` purchase
//! (0.0005 USDC per request, paid by the account) and sends it to whatever
//! `RIVERBIT_BASE_URL` points at, then prints the receipt and the quota again.
//!
//! If the purchase outcome is unknown the example does not sign it again: it
//! reads the quota back and you compare the `surplus` and `used` counts with
//! the values printed before.

mod common;

use riverbit_mm_sdk::actions::ReserveRequestWeight;
use riverbit_mm_sdk::{Address, Error, InfoClient};

async fn print_quota(info: &InfoClient, account: &Address, label: &str) {
    match info.user_rate_limit(account).await {
        Ok(q) => println!(
            "{label}: cap {} used {} surplus {} cumVlm {} -> {} requests remaining",
            q.n_requests_cap,
            q.n_requests_used,
            q.n_requests_surplus,
            q.cum_vlm,
            q.remaining()
        ),
        Err(e) => eprintln!("{label}: userRateLimit failed: {e}"),
    }
}

#[tokio::main]
async fn main() {
    let env = common::Env::load();
    let info = env.info();
    let master = env.master(&info).await;
    print_quota(&info, &master, "quota").await;

    let Ok(count) = std::env::var("BUY_REQUESTS") else {
        println!("set BUY_REQUESTS=<n> to buy requests; nothing was signed");
        return;
    };
    let weight: u64 = count.trim().parse().unwrap_or_else(|_| {
        eprintln!("BUY_REQUESTS must be a whole number");
        std::process::exit(2)
    });
    let purchase = ReserveRequestWeight::new(weight);
    println!(
        "buying {weight} requests for {} micro-USDC",
        purchase.cost_micro_usdc()
    );

    let domain = env.domain(&info).await;
    let exchange = env.exchange(domain);
    match exchange.send(&purchase).await {
        Ok(receipt) => println!("purchase accepted (default ack: {})", receipt.default_ack),
        Err(Error::Rejected(r)) => eprintln!("purchase rejected, nothing bought: {r}"),
        Err(Error::UnknownOutcome(unknown)) => {
            // There is no order to look up. Do not sign again; compare quotas.
            eprintln!(
                "unknown outcome ({}), nonce {}; compare the quota below with the one above",
                unknown.reason,
                unknown.nonce()
            );
        }
        Err(e) => eprintln!("{e}"),
    }
    print_quota(&info, &master, "quota after").await;
}
