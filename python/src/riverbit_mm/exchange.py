"""``/exchange`` client: sign, submit over REST or bin-ws, parse receipts."""

from __future__ import annotations

import json
from collections.abc import Sequence
from typing import TYPE_CHECKING, Any

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
    action_type,
)
from .errors import (
    NotConnected,
    NotSent,
    PostError,
    RequestTimeout,
    SessionClosed,
    UnknownOutcome,
)
from .info import InfoClient
from .nonce import NonceSource, shared_nonce_source
from .receipts import Receipt, interpret_reply
from .signing import DEFAULT_SOURCE, SignedRequest, Signer, normalize_address

if TYPE_CHECKING:
    from .ws import BinWsSession

#: One submission is bounded by this many seconds. Running out of time leaves the
#: outcome unknown: the caller reconciles, it never re-signs.
EXCHANGE_TIMEOUT = 20.0


def _leg_count(action_json: bytes) -> int:
    obj = json.loads(action_json)
    for key in ("orders", "cancels", "modifies"):
        if isinstance(obj.get(key), list):
            return len(obj[key])
    return 1


class ExchangeClient:
    """Signs actions and submits them, one signer at a time.

    ``base_url`` is explicit (for example ``https://pre-api.riverbit.io``). Pass
    ``ws=`` a logged-in :class:`~riverbit_mm.ws.BinWsSession` to carry actions over
    bin-ws (``action_json``) instead of ``POST /exchange``; the signed bytes are
    identical either way.

    ``chain_id`` is ``nextAgentChainId``; if omitted it is fetched once from
    ``chainIdentity``. ``vault_address`` makes every action trade for that vault
    (the signer must be its leader or the leader's API wallet).

    **Errors.** :meth:`submit` raises :class:`~riverbit_mm.ActionRejected` when the
    whole action was rejected, :class:`~riverbit_mm.UnknownOutcome` when the outcome
    cannot be known, and :class:`~riverbit_mm.NotSent` only when the bin-ws session
    was provably not connected. Individual leg rejections are not exceptions: they
    are in :attr:`Receipt.failed`.
    """

    def __init__(
        self,
        base_url: str,
        signer: Signer,
        *,
        chain_id: int | None = None,
        vault_address: str | None = None,
        nonce_source: NonceSource | None = None,
        ws: BinWsSession | None = None,
        http_client: httpx.AsyncClient | None = None,
        timeout: float = EXCHANGE_TIMEOUT,
        source: str = DEFAULT_SOURCE,
        sleep: SleepFn | None = None,
    ) -> None:
        self.signer = signer
        self.chain_id = chain_id
        self.vault_address = normalize_address(vault_address) if vault_address else None
        self.nonce_source = nonce_source or shared_nonce_source(signer.address_lower)
        self.ws = ws
        self.timeout = timeout
        self.source = source
        self._core = HttpCore(base_url, http_client=http_client, timeout=timeout, sleep=sleep)
        self.info = InfoClient(base_url, _core=self._core)

    async def __aenter__(self) -> ExchangeClient:
        return self

    async def __aexit__(self, *exc: object) -> None:
        await self.aclose()

    async def aclose(self) -> None:
        await self._core.aclose()

    @property
    def transport(self) -> str:
        return "bin-ws" if self.ws is not None else "rest"

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
        return self.signer.sign_action(
            action_json,
            self.nonce_source.next(),
            chain_id=self.chain_id,
            vault_address=self.vault_address,
            source=self.source,
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
        under the same nonce; do that only after :class:`NotSent`.
        """
        kind = action_type(request.action_json)
        expected = _leg_count(request.action_json)
        reply = await (self._send_ws(request) if self.ws is not None else self._send_rest(request))
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

    async def _send_ws(self, request: SignedRequest) -> Any:
        assert self.ws is not None
        try:
            return await self.ws.post_action(request.body, timeout=self.timeout)
        except NotConnected as exc:
            raise NotSent(str(exc), request=request) from exc
        except (SessionClosed, RequestTimeout, PostError) as exc:
            raise UnknownOutcome(str(exc), request=request, transport="bin-ws", cause=exc) from exc

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
