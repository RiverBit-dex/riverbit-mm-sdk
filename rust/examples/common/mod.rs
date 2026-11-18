//! Shared setup for the examples. Settings come from environment variables;
//! the private key is read from a file, never from the command line or an
//! environment variable, so it does not end up in shell history or `ps`.
//!
//! | Variable | Meaning | Default |
//! |---|---|---|
//! | `RIVERBIT_KEY_FILE` | path of a file holding the API wallet (or master) private key, hex, one line | required |
//! | `RIVERBIT_BASE_URL` | REST base URL | `https://pre-api.riverbit.io` |
//! | `RIVERBIT_WS_URL` | bin-ws URL | `wss://pre-api.riverbit.io/bin-ws` |
//! | `RIVERBIT_LOGIN_HOST` | host line of the login message | `pre-api.riverbit.io` |
//! | `RIVERBIT_COIN` | market | `BTC` |

#![allow(dead_code)]

use riverbit_mm_sdk::info::UserRole;
use riverbit_mm_sdk::{Address, BinWsConfig, InfoClient, Signer, SigningDomain};

pub struct Env {
    pub base_url: String,
    pub ws_url: String,
    pub login_host: String,
    pub coin: String,
    pub signer: Signer,
}

fn var(name: &str, default: &str) -> String {
    std::env::var(name).unwrap_or_else(|_| default.to_string())
}

pub fn required(name: &str) -> String {
    std::env::var(name).unwrap_or_else(|_| {
        eprintln!("set {name}");
        std::process::exit(2)
    })
}

impl Env {
    pub fn load() -> Env {
        let path = required("RIVERBIT_KEY_FILE");
        let key = std::fs::read_to_string(&path).unwrap_or_else(|e| {
            eprintln!("cannot read the key file named by RIVERBIT_KEY_FILE: {e}");
            std::process::exit(2)
        });
        Env {
            base_url: var("RIVERBIT_BASE_URL", "https://pre-api.riverbit.io"),
            ws_url: var("RIVERBIT_WS_URL", "wss://pre-api.riverbit.io/bin-ws"),
            login_host: var("RIVERBIT_LOGIN_HOST", "pre-api.riverbit.io"),
            coin: var("RIVERBIT_COIN", "BTC"),
            signer: Signer::from_hex(&key)
                .expect("the key file must hold the private key as 32 bytes of hex"),
        }
    }

    pub fn ws_config(&self) -> BinWsConfig {
        BinWsConfig::new(
            self.ws_url.clone(),
            self.login_host.clone(),
            self.signer.clone(),
        )
    }

    pub fn info(&self) -> InfoClient {
        InfoClient::new(&self.base_url).expect("build the info client")
    }

    /// The account the signer trades for: the master behind an API wallet, or
    /// the signer itself when it is a master. `userRole` is expensive, so a
    /// real program calls it once at startup, as this does.
    pub async fn master(&self, info: &InfoClient) -> Address {
        match info
            .user_role(&self.signer.address())
            .await
            .expect("userRole")
        {
            UserRole::Agent { user } => user.parse().expect("master address"),
            UserRole::User => self.signer.address(),
            other => panic!("this signer cannot trade: {other:?}"),
        }
    }

    /// The signing domain: always read the chain ID from `chainIdentity`.
    pub async fn domain(&self, info: &InfoClient) -> SigningDomain {
        SigningDomain::new(
            info.chain_identity()
                .await
                .expect("chainIdentity")
                .next_agent_chain_id,
        )
    }
}
