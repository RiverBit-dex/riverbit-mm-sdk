//! Streams the incremental `l2` book of one market and prints the best bid
//! and ask whenever they change.
//!
//! ```text
//! RIVERBIT_KEY_FILE=./key cargo run --example stream_market_data
//! ```
//!
//! Market data needs no account authority, but every connection still logs in
//! first. The session reconnects on its own; a new snapshot replaces the book.

mod common;

use riverbit_mm_sdk::{BinWsSession, Book, Event, Subscription};

#[tokio::main]
async fn main() {
    let env = common::Env::load();
    let (session, mut events) = BinWsSession::connect(env.ws_config())
        .await
        .expect("connect and log in");
    println!(
        "logged in as {} (account {})",
        session.signer(),
        session.user()
    );
    session
        .subscribe(&Subscription::l2(&env.coin))
        .await
        .expect("subscribe");

    let mut book = Book::new(env.coin.clone());
    let mut last = None;
    while let Some(event) = events.next().await {
        match event {
            Event::Frame(frame) => {
                match book.apply_frame(&frame) {
                    Ok(true) => {}
                    Ok(false) => continue, // acknowledgment, pong or another stream
                    Err(e) => {
                        // The book cannot follow the stream: resubscribe for a fresh snapshot.
                        eprintln!("{e}; resubscribing");
                        let l2 = Subscription::l2(&env.coin);
                        session.unsubscribe(&l2).await.ok();
                        session.subscribe(&l2).await.ok();
                        continue;
                    }
                }
                let best = (
                    book.best_bid().map(|l| (l.px.clone(), l.sz.clone())),
                    book.best_ask().map(|l| (l.px.clone(), l.sz.clone())),
                );
                if last.as_ref() != Some(&best) {
                    let show = |side: &Option<(String, String)>| match side {
                        Some((px, sz)) => format!("{px} x {sz}"),
                        None => "-".to_string(),
                    };
                    println!("{}  bid {}  ask {}", env.coin, show(&best.0), show(&best.1));
                    last = Some(best);
                }
            }
            Event::Disconnected { reason } => eprintln!("disconnected: {reason}"),
            Event::Reconnected { attempts, .. } => {
                println!("reconnected after {attempts} attempt(s); waiting for a new snapshot")
            }
            Event::Closed { reason } => {
                eprintln!("closed: {reason}");
                break;
            }
        }
    }
}
