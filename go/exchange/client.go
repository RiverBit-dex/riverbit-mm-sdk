package exchange

import (
	"bytes"
	"context"
	"encoding/binary"
	"encoding/hex"
	"encoding/json"
	"errors"
	"fmt"
	"log/slog"
	"net/http"
	"strings"
	"sync"
	"time"

	"github.com/ethereum/go-ethereum/crypto"
	"github.com/vmihailenco/msgpack/v5"
)

// SigningSource is the "source" string of the EIP-712 Agent message. It is
// "b" in every environment.
const SigningSource = "b"

// ErrNoChainID is returned when a Client signs before SignChainID is set.
var ErrNoChainID = errors.New("SignChainID is not set: query chainIdentity and set Client.SignChainID (see Client.LoadChainID)")

// Client signs and submits trading actions for one signer and one account (the
// master account, or a vault the master leads), over REST or an authenticated
// bin-ws session, and reconciles every request whose outcome is unknown.
//
// Set the exported fields after construction and before the first call. A
// Client is safe for use by several goroutines; submissions of one signer are
// serialized. The Last* fields describe the most recent Exchange or
// RecoverPending call and are not synchronized with concurrent calls.
type Client struct {
	// URL is the REST base URL, for example "https://pre-api.riverbit.io".
	URL string
	// HTTP is the HTTP client used for REST; New sets one with a 15 second timeout.
	HTTP *http.Client
	// WS, when set, carries signed actions over a logged-in bin-ws session
	// (action_json) instead of POST <URL>/exchange. Both paths send the same
	// signed bytes. *binws.Session implements ActionTransport.
	WS ActionTransport
	// Queries, when set, carries the reconciliation queries (orderStatus,
	// openOrders) over bin-ws info_json instead of POST <URL>/info. Startup
	// queries (chainIdentity, metaAndAssetCtxs, userRole, vault checks) always
	// use REST. *binws.Session implements InfoTransport.
	Queries InfoTransport
	// SignChainID is the EIP-712 domain chain ID: nextAgentChainId from the
	// chainIdentity query. It has no default. A wrong value makes the node
	// recover a random signer and reject every action with code 91.
	SignChainID uint64
	// StateDir enables the durable pending journal: a directory, private to
	// this signer and shared by every process that uses it, holding the nonce
	// floor, the write-ahead request journal and the audit log. Empty disables
	// durability (a vault or subaccount client requires it).
	StateDir string
	// RequireExecutionHeight makes reconciliation fail closed unless every
	// orderStatus answer carries a height that is not behind the highest
	// confirmed one. The height is persisted in StateDir.
	RequireExecutionHeight bool
	// Logger receives progress records; nil is silent. Private keys and full
	// signed requests are never logged.
	Logger *slog.Logger

	// LastOrderIDs and LastStatuses hold the order IDs and states observed for
	// the most recently resolved action; LastExecutionHeight is the highest
	// orderStatus height seen; LastReceipt is the parsed reply of the last
	// submission, if it carried one.
	LastOrderIDs        []uint64
	LastStatuses        []string
	LastExecutionHeight uint64
	LastReceipt         *Receipt

	signer    *Signer
	account   string
	principal string
	// vaultOperator marks account as a registered vault that this signer trades
	// for as the vault leader or the leader's agent, not as a master owning a
	// subaccount. VerifySubAccount then checks the vault registry and leader.
	vaultOperator bool
	submitMu      sync.Mutex
	nonce         uint64
}

// New returns a Client that signs with signer and trades for signer's own
// account (or the master set with SetPrincipal).
func New(url string, signer *Signer) (*Client, error) {
	if signer == nil || signer.key == nil {
		return nil, fmt.Errorf("signer is required")
	}
	return &Client{URL: strings.TrimRight(url, "/"), HTTP: &http.Client{Timeout: 15 * time.Second}, signer: signer}, nil
}

// NewForVault returns a Client that signs as signer for a vault. signer must be
// the vault leader or the leader's authorized API wallet; call VerifySubAccount
// before submitting any action.
func NewForVault(url string, signer *Signer, vault string) (*Client, error) {
	target, err := normalizeAddress(vault)
	if err != nil {
		return nil, err
	}
	c, err := New(url, signer)
	if err != nil {
		return nil, err
	}
	c.account, c.vaultOperator = target, true
	return c, nil
}

// NewForSubAccount returns a Client that signs as signer, the master, for one
// of its subaccounts. The target is fixed for the lifetime of the client, so no
// setter can silently change the identity of an outstanding durable request.
func NewForSubAccount(url string, signer *Signer, account string) (*Client, error) {
	target, err := normalizeAddress(account)
	if err != nil {
		return nil, err
	}
	c, err := New(url, signer)
	if err != nil {
		return nil, err
	}
	if target == c.SignerAddress() {
		return nil, fmt.Errorf("subaccount must differ from signer")
	}
	c.account = target
	return c, nil
}

// Signer returns the signer, for example to pass to binws.Config.Sign.
func (c *Client) Signer() *Signer { return c.signer }

// SignerAddress returns the lowercase address of the signing key.
func (c *Client) SignerAddress() string { return c.signer.Address() }

// AccountAddress returns the account queries and subscriptions must use: the
// vault or subaccount, else the principal set by SetPrincipal, else the signer.
func (c *Client) AccountAddress() string {
	if c.account != "" {
		return c.account
	}
	if c.principal != "" {
		return c.principal
	}
	return c.SignerAddress()
}

// SetPrincipal records the master account that an API wallet trades for (for
// example the user resolved by the bin-ws login). Only for clients without a
// vault or subaccount target; it changes which account is queried, not the
// signature.
func (c *Client) SetPrincipal(address string) error {
	if c.account != "" {
		return fmt.Errorf("principal cannot be set on a vault or subaccount client")
	}
	target, err := normalizeAddress(address)
	if err != nil {
		return err
	}
	c.principal = target
	return nil
}

func normalizeAddress(address string) (string, error) {
	if len(address) != 42 || !strings.HasPrefix(address, "0x") {
		return "", fmt.Errorf("invalid account address")
	}
	b, err := hex.DecodeString(address[2:])
	if err != nil || len(b) != 20 || bytes.Equal(b, make([]byte, 20)) {
		return "", fmt.Errorf("invalid account address")
	}
	return strings.ToLower(address), nil
}

func (c *Client) logInfo(msg string, args ...any) {
	if c.Logger != nil {
		c.Logger.Info(msg, args...)
	}
}

// isDefaultAck accepts exactly {"type":"default"}; any additional field is rejected.
func isDefaultAck(response json.RawMessage) bool {
	var fields map[string]json.RawMessage
	if json.Unmarshal(response, &fields) != nil || len(fields) != 1 {
		return false
	}
	var typ string
	return json.Unmarshal(fields["type"], &typ) == nil && typ == "default"
}

var signerOperations sync.Map

func (c *Client) signerOperationLock() *sync.Mutex {
	value, _ := signerOperations.LoadOrStore(c.SignerAddress(), new(sync.Mutex))
	return value.(*sync.Mutex)
}

var signerNonces = struct {
	sync.Mutex
	values map[string]uint64
}{values: make(map[string]uint64)}

// Nonces are shared by all clients with the same key in this process. Separate
// processes must use a single signer owner; this is not a distributed allocator.
func (c *Client) nextNonce() uint64 {
	signerNonces.Lock()
	defer signerNonces.Unlock()
	identity := c.SignerAddress()
	n := uint64(time.Now().UnixMilli())
	if n <= signerNonces.values[identity] {
		n = signerNonces.values[identity] + 1
	}
	if n <= c.nonce {
		n = c.nonce + 1
	}
	c.nonce = n
	signerNonces.values[identity] = n
	return n
}

// NextNonce returns a fresh millisecond-timestamp nonce, unique within this
// process for the signer. With a StateDir it is also persisted as the durable
// nonce floor.
func (c *Client) NextNonce() (uint64, error) {
	lock := c.signerOperationLock()
	lock.Lock()
	defer lock.Unlock()
	release, err := c.lockDurableSigner()
	if err != nil {
		return 0, err
	}
	defer release()
	return c.durableNonce()
}

func word(n uint64) []byte        { b := make([]byte, 32); binary.BigEndian.PutUint64(b[24:], n); return b }
func hash(parts ...[]byte) []byte { return crypto.Keccak256(parts...) }

func domain(name string, chain uint64) []byte {
	return hash(hash([]byte("EIP712Domain(string name,string version,uint256 chainId,address verifyingContract)")), hash([]byte(name)), hash([]byte("1")), word(chain), make([]byte, 32))
}

// msgpackAction encodes action as MessagePack in struct field order with
// compact integers: the bytes the node derives from the received JSON.
func msgpackAction(action any) ([]byte, error) {
	var buf bytes.Buffer
	enc := msgpack.NewEncoder(&buf)
	enc.UseCompactInts(true)
	if err := enc.Encode(action); err != nil {
		return nil, err
	}
	return buf.Bytes(), nil
}

// connectionID is keccak256(msgpack(action) || nonce u64 big-endian || tail),
// where tail is 0x00 without a target account, or 0x01 followed by the 20-byte
// vault (or subaccount) address when the request carries vaultAddress.
func (c *Client) connectionID(action any, nonce uint64) ([]byte, error) {
	packed, err := msgpackAction(action)
	if err != nil {
		return nil, err
	}
	buf := bytes.NewBuffer(packed)
	b := make([]byte, 8)
	binary.BigEndian.PutUint64(b, nonce)
	buf.Write(b)
	if c.account == "" {
		buf.WriteByte(0)
	} else {
		target, err := hex.DecodeString(c.account[2:])
		if err != nil || len(target) != 20 {
			return nil, fmt.Errorf("invalid vault address for connectionId")
		}
		buf.WriteByte(1)
		buf.Write(target)
	}
	return hash(buf.Bytes()), nil
}

func (c *Client) signingDigest(action any, nonce uint64) ([]byte, error) {
	if c.SignChainID == 0 {
		return nil, ErrNoChainID
	}
	connectionID, err := c.connectionID(action, nonce)
	if err != nil {
		return nil, err
	}
	agent := hash(hash([]byte("Agent(string source,bytes32 connectionId)")), hash([]byte(SigningSource)), connectionID)
	return hash([]byte{0x19, 0x01}, domain("Exchange", c.SignChainID), agent), nil
}

// Sign signs action with nonce: the EIP-712 phantom-agent signature over the
// connection ID of the MessagePack-encoded action, bound to the client's vault
// or subaccount if it has one. Send the same action bytes you signed.
func (c *Client) Sign(action any, nonce uint64) (Signature, error) {
	digest, err := c.signingDigest(action, nonce)
	if err != nil {
		return Signature{}, err
	}
	return c.signer.signDigest(digest)
}

// requestEnvelope is the /exchange request body. Field order is fixed.
type requestEnvelope struct {
	Action       any       `json:"action"`
	Nonce        uint64    `json:"nonce"`
	Signature    Signature `json:"signature"`
	VaultAddress string    `json:"vaultAddress,omitempty"`
}

// SignedRequest signs action with nonce and returns the complete request body:
// the exact bytes to POST to /exchange or to send as bin-ws action_json.
func (c *Client) SignedRequest(action any, nonce uint64) ([]byte, error) {
	sig, err := c.Sign(action, nonce)
	if err != nil {
		return nil, err
	}
	return json.Marshal(requestEnvelope{action, nonce, sig, c.account})
}

// LoadChainID queries chainIdentity over REST and sets SignChainID.
func (c *Client) LoadChainID(ctx context.Context) error {
	id, err := c.InfoClient().ChainIdentity(ctx)
	if err != nil {
		return fmt.Errorf("read chain signing identity: %w", err)
	}
	if id.SignChainID() == 0 {
		return fmt.Errorf("gateway chainIdentity has no agentChainId")
	}
	c.SignChainID = id.SignChainID()
	return nil
}

// InfoClient returns a REST /info client that shares the Client's URL and HTTP
// client.
func (c *Client) InfoClient() *InfoClient { return &InfoClient{URL: c.URL, HTTP: c.HTTP} }

// Info queries POST <URL>/info and decodes the answer into out. It always uses
// REST; see InfoClient.
func (c *Client) Info(ctx context.Context, body, out any) error {
	return c.InfoClient().Query(ctx, body, out)
}

// retryRateLimited repeats call with the server's Retry-After, or capped
// exponential backoff, while it fails with a *RateLimitError. When the context
// ends first it returns the context error joined with the last rate-limit error.
func retryRateLimited(ctx context.Context, call func() error) error {
	delay := 250 * time.Millisecond
	for {
		err := call()
		var limit *RateLimitError
		if err == nil || !errors.As(err, &limit) {
			return err
		}
		wait := delay
		if limit.RetryAfter > wait {
			wait = limit.RetryAfter
		}
		timer := time.NewTimer(wait)
		select {
		case <-ctx.Done():
			timer.Stop()
			return fmt.Errorf("%w: %w", ctx.Err(), err)
		case <-timer.C:
		}
		if delay < 8*time.Second {
			delay = min(delay*2, 8*time.Second)
		}
	}
}

// orderQuery serves the reconciliation queries (orderStatus, openOrders):
// over bin-ws info_json when Queries is set, otherwise over REST /info.
func (c *Client) orderQuery(ctx context.Context, body, out any) error {
	if c.Queries != nil {
		return c.wsInfo(ctx, body, out)
	}
	return c.Info(ctx, body, out)
}

// OpenOrders lists the open orders of AccountAddress, through Queries if set,
// otherwise REST. An empty list means no current open orders; it does not
// prove that a request never executed.
func (c *Client) OpenOrders(ctx context.Context) ([]OpenOrder, error) {
	var orders []OpenOrder
	err := c.orderQuery(ctx, map[string]any{"type": "openOrders", "user": c.AccountAddress()}, &orders)
	return orders, err
}

// Exchange signs action with a fresh nonce, submits it and reconciles it.
// action is a *Place, *Cancel, *CancelByCloid, *Modify or *BatchModify (the
// value forms work too).
//
// It returns nil only when the action is known to have executed. A whole-action
// rejection is an *ActionRejectedError, or wraps ErrExecutionRejected when a
// StateDir is configured. If the outcome is unknown it returns an
// *UnknownOutcomeError; the request is never re-signed or resent. With a
// StateDir the request stays in the pending journal until RecoverPending
// resolves it, and a new Exchange is refused until then.
//
// A StateDir is strongly recommended. Orders without a Cloid get a generated
// one so their outcome can be looked up. Callers must still reconcile
// openOrders after every mutation or timeout.
func (c *Client) Exchange(ctx context.Context, action any) error {
	lock := c.signerOperationLock()
	lock.Lock()
	defer lock.Unlock()
	release, err := c.lockDurableSigner()
	if err != nil {
		return err
	}
	defer release()
	if err := c.restoreExecutionHeight(); err != nil {
		return err
	}
	if c.StateDir != "" {
		return c.trackedExchange(ctx, action)
	}
	n, err := c.durableNonce()
	if err != nil {
		return err
	}
	return c.exchangeNonce(ctx, action, n)
}

func (c *Client) exchangeNonce(ctx context.Context, action any, n uint64) error {
	digest, err := c.signingDigest(action, n)
	if err != nil {
		return err
	}
	s, err := c.signer.signDigest(digest)
	if err != nil {
		return err
	}
	rawSignature, err := signatureBytes(s)
	if err != nil {
		return err
	}
	publicKey, err := crypto.SigToPub(digest, rawSignature)
	if err != nil {
		return fmt.Errorf("recover signer from action signature: %w", err)
	}
	recovered := strings.ToLower(crypto.PubkeyToAddress(*publicKey).Hex())
	if recovered != c.SignerAddress() {
		return fmt.Errorf("action signature signer mismatch: recovered %s", recovered)
	}
	c.logInfo("action signed", "signer", recovered, "nonce", n)
	return c.submit(ctx, action, n, s)
}

// unknown wraps cause as an unknown outcome of the request signed with nonce n.
func (c *Client) unknown(action any, n uint64, cause error) error {
	raw, _ := json.Marshal(action)
	return &UnknownOutcomeError{Nonce: n, Account: c.AccountAddress(), Action: raw, Cloids: actionCloids(action), Cause: cause}
}

func actionCloids(action any) []string {
	var out []string
	add := func(o Order) {
		if o.Cloid != "" {
			out = append(out, o.Cloid)
		}
	}
	switch a := action.(type) {
	case Place:
		for _, o := range a.Orders {
			add(o)
		}
	case *Place:
		for _, o := range a.Orders {
			add(o)
		}
	case Modify:
		add(a.Order)
	case *Modify:
		add(a.Order)
	case BatchModify:
		for _, m := range a.Modifies {
			add(m.Order)
		}
	case *BatchModify:
		for _, m := range a.Modifies {
			add(m.Order)
		}
	case CancelByCloid:
		for _, x := range a.Cancels {
			out = append(out, x.Cloid.String())
		}
	case *CancelByCloid:
		for _, x := range a.Cancels {
			out = append(out, x.Cloid.String())
		}
	}
	return out
}

func (c *Client) submit(ctx context.Context, action any, n uint64, s Signature) error {
	var result struct {
		Status          string          `json:"status"`
		Response        json.RawMessage `json:"response"`
		CommittedResult json.RawMessage `json:"committedResult"`
	}
	// One serialization for both transports: REST POST /exchange and bin-ws
	// action_json carry exactly these bytes (struct order = signing key order).
	body, err := json.Marshal(requestEnvelope{action, n, s, c.account})
	if err != nil {
		return err
	}
	var reply json.RawMessage
	if c.WS != nil {
		err = c.postBinary(ctx, body, &reply)
	} else {
		err = c.postREST(ctx, body, &reply)
	}
	if err != nil {
		return c.unknown(action, n, err)
	}
	if err = json.Unmarshal(reply, &result); err != nil {
		return c.unknown(action, n, fmt.Errorf("invalid exchange response: %w", err))
	}
	c.LastReceipt, _ = ParseReceipt(reply)
	// Preserve complete per-leg results before an individual error discards
	// them. A rejected replacement has no new cloid to discover via /info.
	if c.StateDir != "" {
		count := 0
		switch batch := action.(type) {
		case BatchModify:
			count = len(batch.Modifies)
		case *BatchModify:
			count = len(batch.Modifies)
		}
		raw := result.Response
		if result.Status == "err" {
			raw = result.CommittedResult
		}
		if count > 0 && (result.Status == "ok" || result.Status == "err") {
			if _, e := parseBatchReceipt(raw, count); e == nil {
				return &batchReceipt{Raw: raw}
			}
		}
		if count := cancelCount(action); count > 0 && (result.Status == "ok" || result.Status == "err") {
			if _, e := parseCancelReceipt(raw, count); e == nil {
				return &cancelReceipt{Raw: raw}
			}
		}
	}
	if result.Status != "ok" {
		if code, ok := finalRejection(result.Status, result.Response, result.CommittedResult); ok {
			return &ActionRejectedError{Status: result.Status, Response: result.Response, Code: code}
		}
		return c.unknown(action, n, fmt.Errorf("exchange: %s %s", result.Status, result.Response))
	}
	var response struct {
		Data struct {
			Statuses []json.RawMessage `json:"statuses"`
		} `json:"data"`
	}
	if err = json.Unmarshal(result.Response, &response); err != nil {
		return c.unknown(action, n, fmt.Errorf("exchange response: %s", result.Response))
	}
	if len(response.Data.Statuses) == 0 {
		// An accepted on-chain modify returns only {"type":"default"}; rejections carry
		// data.statuses. Acceptance is not final: callers must confirm via /info.
		// Missing statuses for other actions remains an error.
		if isModifyAction(action) && isDefaultAck(result.Response) {
			return nil
		}
		return c.unknown(action, n, fmt.Errorf("exchange missing statuses: %s", result.Response))
	}
	for _, raw := range response.Data.Statuses {
		if string(raw) == `"success"` {
			continue
		}
		var status struct {
			Error   string `json:"error"`
			Resting *struct {
				OID uint64 `json:"oid"`
			} `json:"resting"`
			Filled *struct {
				OID uint64 `json:"oid"`
			} `json:"filled"`
		}
		if err := json.Unmarshal(raw, &status); err != nil {
			return fmt.Errorf("exchange unknown status: %s", raw)
		}
		if status.Error != "" {
			return fmt.Errorf("exchange: %s", status.Error)
		}
		if status.Resting == nil && status.Filled == nil {
			return fmt.Errorf("exchange unconfirmed status: %s", raw)
		}
	}
	return nil
}

func isModifyAction(action any) bool {
	switch action.(type) {
	case Modify, *Modify, BatchModify, *BatchModify:
		return true
	default:
		return false
	}
}

// VerifySubAccount checks the published chain ownership relation before the
// client is used: the vault is registered, active and led by the signer or by
// the master the signer is an API wallet of, or the subaccount belongs to the
// signer. An empty list, malformed relation or unavailable API fails closed.
// It does nothing for a client without a vault or subaccount target.
func (c *Client) VerifySubAccount(ctx context.Context) error {
	if c.account == "" {
		return nil
	}
	if c.vaultOperator {
		return c.verifyVaultOperator(ctx)
	}
	var accounts []struct {
		Name   string `json:"name"`
		User   string `json:"subAccountUser"`
		Master string `json:"master"`
	}
	if err := c.Info(ctx, map[string]any{"type": "subAccounts", "user": c.SignerAddress()}, &accounts); err != nil {
		return err
	}
	found := false
	for _, a := range accounts {
		user, err := normalizeAddress(a.User)
		if err != nil {
			return fmt.Errorf("invalid subaccount relation")
		}
		master, err := normalizeAddress(a.Master)
		if err != nil || master != c.SignerAddress() {
			return fmt.Errorf("subaccount master mismatch")
		}
		if user == c.account {
			if found {
				return fmt.Errorf("duplicate subaccount relation")
			}
			found = true
		}
	}
	if !found {
		return fmt.Errorf("subaccount is not registered to signer")
	}
	return nil
}

// verifyVaultOperator fails closed unless the target is an active registered
// vault whose leader is the signer or has approved the signer as its agent.
// Every registered class (protocol / guild / priority / user) trades only
// through the leader's API.
func (c *Client) verifyVaultOperator(ctx context.Context) error {
	var vault struct {
		Address string `json:"vaultAddress"`
		Status  string `json:"status"`
		Class   string `json:"class"`
	}
	if err := c.Info(ctx, map[string]any{"type": "vaultRegistry", "vault": c.account}, &vault); err != nil {
		return err
	}
	addr, err := normalizeAddress(vault.Address)
	if err != nil || addr != c.account {
		return fmt.Errorf("vault registry address mismatch")
	}
	if vault.Status != "normal" {
		return fmt.Errorf("vault is not normal: %q", vault.Status)
	}
	switch vault.Class {
	case "protocol", "priority", "guild", "user":
		return c.verifyVaultLeader(ctx)
	default:
		return fmt.Errorf("vault class %q has no known executor", vault.Class)
	}
}

func (c *Client) verifyVaultLeader(ctx context.Context) error {
	var details struct {
		Leader string `json:"leader"`
	}
	if err := c.Info(ctx, map[string]any{"type": "vaultDetails", "vaultAddress": c.account}, &details); err != nil {
		return err
	}
	leader, err := normalizeAddress(details.Leader)
	if err != nil {
		return fmt.Errorf("invalid vault leader")
	}
	if leader == c.SignerAddress() {
		return nil
	}
	role, err := c.InfoClient().UserRole(ctx, c.SignerAddress())
	if err != nil {
		return err
	}
	if role.Role == "agent" {
		if principal, err := normalizeAddress(role.Data.User); err == nil && principal == leader {
			return nil
		}
	}
	return fmt.Errorf("signer %s is neither the vault leader nor its agent", c.SignerAddress())
}
