"""``/exchange`` client: sign, submit with REST ``POST /exchange``, parse receipts."""

from __future__ import annotations

import json
from collections.abc import Sequence
from typing import Any

import httpx

from ._http import HttpCore, SleepFn, parse_retry_after
from .actions import (
    Action,
    BatchModify,
    Cancel,
    CancelByCloid,
    CancelByCloidItem,
    CancelItem,
    Modify,
    ModifyItem,
    Order,
    Place,
    ReserveRequestWeight,
    action_type,
)
from .errors import UnknownOutcome
from .info import InfoClient
from .nonce import NonceSource, shared_nonce_source
from .receipts import Receipt, interpret_reply
from .signing import DEFAULT_SOURCE, SignedRequest, Signer, normalize_address

#: One submission is bounded by this many seconds. Running out of time leaves the
#: outcome unknown: the caller reconciles, it never re-signs.
EXCHANGE_TIMEOUT = 20.0


def _leg_count(action_json: bytes) -> int:
    obj = json.loads(action_json)
    if obj.get("type") == ReserveRequestWeight.TYPE:
        return 0  # a purchase has no legs
    for key in ("orders", "cancels", "modifies"):
        if isinstance(obj.get(key), list):
            return len(obj[key])
    return 1


class ExchangeClient:
    """Signs actions and submits them, one signer at a time.

    ``base_url`` is explicit (for example ``https://pre-api.riverbit.io``). Orders,
    cancellations and modifications are submitted with REST ``POST /exchange``;
    bin-ws carries market data, order and fill events and queries.

    ``gateway_token`` is an optional gateway credential. When set, every REST request
    (``/exchange`` and ``/info``) carries it in the ``X-RiverBit-MM-Token`` header; it
    does not change signing or nonces and is never logged or shown in ``repr``.

    ``chain_id`` is ``nextAgentChainId``; if omitted it is fetched once from
    ``chainIdentity``. ``vault_address`` makes every action trade for that vault
    (the signer must be its leader or the leader's API wallet).

    **Errors.** :meth:`submit` raises :class:`~riverbit_mm.ActionRejected` when the
    whole action was rejected, :class:`~riverbit_mm.UnknownOutcome` when the outcome
    cannot be known. Individual leg rejections are not exceptions: they are in
    :attr:`Receipt.failed`.
    """

    def __init__(
        self,
        base_url: str,
        signer: Signer,
        *,
        chain_id: int | None = None,
        vault_address: str | None = None,
        nonce_source: NonceSource | None = None,
        http_client: httpx.AsyncClient | None = None,
        timeout: float = EXCHANGE_TIMEOUT,
        source: str = DEFAULT_SOURCE,
        sleep: SleepFn | None = None,
        gateway_token: str | None = None,
    ) -> None:
        self.signer = signer
        self.chain_id = chain_id
        self.vault_address = normalize_address(vault_address) if vault_address else None
        self.nonce_source = nonce_source or shared_nonce_source(signer.address_lower)
        self.timeout = timeout
        self.source = source
        self._core = HttpCore(
            base_url,
            http_client=http_client,
            timeout=timeout,
            sleep=sleep,
            gateway_token=gateway_token,
        )
        self.info = InfoClient(base_url, _core=self._core)

    async def __aenter__(self) -> ExchangeClient:
        return self

    async def __aexit__(self, *exc: object) -> None:
        await self.aclose()

    async def aclose(self) -> None:
        await self._core.aclose()

    @property
    def transport(self) -> str:
        return "rest"

    # -- signing ---------------------------------------------------------------------

    async def load_chain_id(self) -> int:
        """Fetch ``nextAgentChainId`` once (no-op when ``chain_id`` is set)."""
        if self.chain_id is None:
            self.chain_id = await self.info.next_agent_chain_id()
        return self.chain_id

    def sign(self, action: Action | bytes) -> SignedRequest:
        """Allocate a nonce and sign (no I/O). ``chain_id`` must be known.

        Persist the returned request before :meth:`submit` if you keep a journal.
        """
        if self.chain_id is None:
            raise RuntimeError("chain_id is unknown: pass chain_id or await load_chain_id() first")
        action_json = action if isinstance(action, bytes) else action.to_json()
        self._check_purchase_destination(action_json)
        return self.signer.sign_action(
            action_json,
            self.nonce_source.next(),
            chain_id=self.chain_id,
            vault_address=self.vault_address,
            source=self.source,
        )

    def _check_purchase_destination(self, action_json: bytes) -> None:
        """A vault-paid purchase goes to the vault: refuse another ``destination``."""
        if self.vault_address is None:
            return
        obj = json.loads(action_json)
        if obj.get("type") == ReserveRequestWeight.TYPE:
            destination = obj.get("destination")
            if destination is not None and destination != self.vault_address:
                raise ValueError(
                    "with vault_address the vault pays and receives: destination must be "
                    "absent or equal to the vault; to have the master pay for a vault, use a "
                    "client without vault_address and set destination to the vault"
                )

    async def prepare(self, action: Action | bytes) -> SignedRequest:
        """:meth:`load_chain_id` if needed, then :meth:`sign`."""
        await self.load_chain_id()
        return self.sign(action)

    # -- submission --------------------------------------------------------------------

    async def post(self, action: Action | bytes) -> Receipt:
        """Sign ``action`` with a fresh nonce and submit it once."""
        return await self.submit(await self.prepare(action))

    async def submit(self, request: SignedRequest) -> Receipt:
        """Send ``request.body`` exactly once and parse the answer.

        Submitting the same :class:`SignedRequest` again re-sends identical bytes
        under the same nonce.
        """
        kind = action_type(request.action_json)
        expected = _leg_count(request.action_json)
        reply = await self._send_rest(request)
        return interpret_reply(
            reply,
            request=request,
            action_type=kind,
            expected_legs=expected,
            transport=self.transport,
        )

    async def _send_rest(self, request: SignedRequest) -> Any:
        try:
            response = await self._core.post_once("/exchange", request.body, self.timeout)
        except httpx.HTTPError as exc:
            raise UnknownOutcome(
                f"{type(exc).__name__}: {exc}", request=request, transport="rest", cause=exc
            ) from exc
        if response.status_code != 200:
            retry_after = parse_retry_after(response.headers.get("Retry-After"))
            raise UnknownOutcome(
                f"HTTP {response.status_code}: {response.text[:300]}",
                request=request,
                transport="rest",
                retry_after=retry_after,
            )
        try:
            return response.json()
        except ValueError as exc:
            raise UnknownOutcome(
                "reply is not valid JSON", request=request, transport="rest", cause=exc
            ) from exc

    # -- conveniences ---------------------------------------------------------------------

    async def place(self, orders: Sequence[Order] | Order, *, grouping: str = "na") -> Receipt:
        legs = [orders] if isinstance(orders, Order) else list(orders)
        return await self.post(Place(legs, grouping))

    async def cancel(self, cancels: Sequence[CancelItem]) -> Receipt:
        return await self.post(Cancel(list(cancels)))

    async def cancel_by_cloid(self, cancels: Sequence[CancelByCloidItem]) -> Receipt:
        return await self.post(CancelByCloid(list(cancels)))

    async def modify(self, oid: int, order: Order) -> Receipt:
        return await self.post(Modify(oid, order))

    async def batch_modify(self, modifies: Sequence[ModifyItem]) -> Receipt:
        return await self.post(BatchModify(list(modifies)))

    async def reserve_request_weight(self, weight: int, destination: str | None = None) -> Receipt:
        """Buy ``weight`` requests (0.0005 USDC each; see :class:`ReserveRequestWeight`).

        ``destination`` is the account that receives them (default: the payer). Never
        called by the SDK itself. An :class:`UnknownOutcome` cannot be looked up by
        oid: compare ``info.user_rate_limit`` before and after, and do not buy again
        until you know.
        """
        return await self.post(ReserveRequestWeight(weight, destination))
