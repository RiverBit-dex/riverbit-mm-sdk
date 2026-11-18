//! The exchange client: sign, send, parse the receipt.

use std::time::Duration;

use crate::actions::Action;
use crate::error::{Error, Result, UnknownOutcome, UnknownReason};
use crate::nonce::NonceSource;
use crate::receipt::{interpret, Receipt};
use crate::signing::{ActionInfo, Address, SignedRequest, Signer, SigningDomain};
use crate::transport::{Rest, RestActions, TransportError};

/// Signs trading actions and submits them with REST `POST /exchange`.
///
/// A request is signed once. The bytes of the [`SignedRequest`] are the bytes
/// that go on the wire. If a send ends without a usable
/// reply the result is [`Error::UnknownOutcome`] carrying that request; the
/// client never signs a replacement with a new nonce.
pub struct ExchangeClient {
    transport: RestActions,
    signer: Signer,
    domain: SigningDomain,
    vault: Option<Address>,
    nonces: NonceSource,
    timeout: Duration,
}

impl ExchangeClient {
    /// A REST client for `base_url`, for example `https://pre-api.riverbit.io`.
    /// `domain` carries the chain ID from `chainIdentity.nextAgentChainId`.
    pub fn new(base_url: &str, signer: Signer, domain: SigningDomain) -> Result<Self> {
        Self::with_http_client(base_url, signer, domain, None)
    }

    /// Like [`new`](Self::new) with your own `reqwest` client. Build it with
    /// `redirect(reqwest::redirect::Policy::none())`: a signed POST must never
    /// be replayed to a redirect target. The default client already does this.
    pub fn with_http_client(
        base_url: &str,
        signer: Signer,
        domain: SigningDomain,
        http: Option<reqwest::Client>,
    ) -> Result<Self> {
        // The per-request HTTP timeout is only a backstop: `with_timeout`
        // bounds the whole submission.
        let transport = RestActions {
            rest: Rest::new(base_url, http)?,
            timeout: Duration::from_secs(300),
        };
        let nonces = NonceSource::for_signer(&signer.address());
        Ok(ExchangeClient {
            transport,
            signer,
            domain,
            vault: None,
            nonces,
            timeout: Duration::from_secs(20),
        })
    }

    /// Trades for a vault: requests carry `vaultAddress` and the
    /// `connectionId` takes the vault tail. The vault must be led by the master
    /// and the signer must be the leader or its authorized API wallet.
    pub fn for_vault(mut self, vault: Address) -> Self {
        self.vault = Some(vault);
        self
    }

    /// Replaces the nonce source. The default is the process-wide source for
    /// the signer, so every client of one key shares one counter.
    pub fn with_nonce_source(mut self, nonces: NonceSource) -> Self {
        self.nonces = nonces;
        self
    }

    /// Bounds one submission. Running out of time is an unknown outcome, even
    /// if the request was never written.
    pub fn with_timeout(mut self, timeout: Duration) -> Self {
        self.timeout = timeout;
        self
    }

    /// Sets the optional gateway credential. Every REST request of this client
    /// then carries it in the `X-RiverBit-MM-Token` header. An empty string
    /// clears it. It is never logged and never part of `Debug` or error
    /// output; characters that are not valid in a header value (CR, LF, spaces,
    /// non-ASCII) are rejected here. It does not change signing, nonces or
    /// the request body: it only lifts gateway rate limits, and a wrong one is
    /// treated as none.
    ///
    /// If you supply your own `reqwest` client it must not follow redirects,
    /// or the credential would go to the redirect target.
    pub fn with_gateway_token(mut self, token: &str) -> Result<Self> {
        self.transport.rest.set_token(token)?;
        Ok(self)
    }

    /// The signing address.
    pub fn signer_address(&self) -> Address {
        self.signer.address()
    }

    /// Signs `action` with a fresh nonce without sending it. Fails before
    /// signing if the action is invalid or does not fit this client's account
    /// (see [`Action::check_account`]).
    pub fn prepare(&self, action: &impl Action) -> Result<SignedRequest> {
        let json = action.to_json()?;
        action.check_account(self.vault.as_ref())?;
        self.signer.sign_request(
            json,
            self.nonces.next(),
            self.vault.as_ref(),
            &self.domain,
            ActionInfo {
                kind: action.kind(),
                expected_legs: action.expected_legs(),
                targets: action.targets(),
            },
        )
    }

    /// Signs and sends `action`, returning its per-leg receipt.
    ///
    /// Check [`Receipt::legs`]: `Ok` means the exchange processed the action,
    /// not that every leg succeeded.
    pub async fn send(&self, action: &impl Action) -> Result<Receipt> {
        let request = self.prepare(action)?;
        self.submit(&request).await
    }

    /// Sends an already signed request. Submitting the same request again is
    /// safe: a duplicate nonce cannot execute twice (it answers with code 205,
    /// which is still an unknown outcome for the original).
    pub async fn submit(&self, request: &SignedRequest) -> Result<Receipt> {
        let unknown = |reason: UnknownReason, reply: Option<String>| {
            Error::UnknownOutcome(Box::new(UnknownOutcome {
                request: request.clone(),
                reason,
                legs: vec![],
                reply,
            }))
        };
        let sent =
            tokio::time::timeout(self.timeout, self.transport.post_action(request.body())).await;
        match sent {
            Err(_elapsed) => Err(unknown(UnknownReason::Timeout, None)),
            Ok(Ok(body)) => interpret(&body, request),
            Ok(Err(err)) => Err(match err {
                TransportError::Timeout => unknown(UnknownReason::Timeout, None),
                TransportError::Disconnected(m) => unknown(UnknownReason::Disconnected(m), None),
                TransportError::SendFailed(m) => unknown(UnknownReason::SendFailed(m), None),
                TransportError::Status {
                    status: 429,
                    body,
                    retry_after,
                } => unknown(UnknownReason::RateLimited { retry_after }, Some(body)),
                TransportError::Status { status, body, .. } => {
                    unknown(UnknownReason::HttpStatus(status), Some(body))
                }
                TransportError::ErrorReply(m) | TransportError::Other(m) => {
                    unknown(UnknownReason::SendFailed(m), None)
                }
            }),
        }
    }
}

impl std::fmt::Debug for ExchangeClient {
    // The signer's own `Debug` shows only its address; the gateway
    // credential is reported as set or unset, never its value.
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("ExchangeClient")
            .field("base", &self.transport.rest.base())
            .field("signer", &self.signer)
            .field("vault", &self.vault)
            .field("gateway_token", &self.transport.rest.has_token())
            .finish()
    }
}
