package exchange

// The pending file is a write-ahead intent, not proof of submission. An
// interrupted/ambiguous send is never retried automatically, even after restart.
import (
	"context"
	"crypto/rand"
	"encoding/hex"
	"encoding/json"
	"errors"
	"fmt"
	"os"
	"path/filepath"
	"strconv"
	"strings"
	"time"
)

// retirePending persists the nonce floor, audits, and removes the journal.
func (c *Client) retirePending(p *pendingRequest, event, detail string) error {
	// Preserve the last durable nonce evidence before removing a legacy
	// journal which may predate the nonce high-water file.
	signerNonces.Lock()
	if p.Nonce > signerNonces.values[c.SignerAddress()] {
		signerNonces.values[c.SignerAddress()] = p.Nonce
	}
	signerNonces.Unlock()
	if _, err := c.durableNonce(); err != nil {
		return fmt.Errorf("pending nonce=%d retained: persist nonce floor: %w", p.Nonce, err)
	}
	if err := c.audit(event, p, detail); err != nil {
		return err
	}
	if err := os.Remove(c.pendingPath()); err != nil {
		return err
	}
	return syncDirectory(c.StateDir)
}

type pendingRequest struct {
	Signer        string                   `json:"signer"`
	Transport     string                   `json:"transport"`
	Account       string                   `json:"account"`
	URL           string                   `json:"url"`
	Nonce         uint64                   `json:"nonce"`
	Action        json.RawMessage          `json:"action"`
	Lookups       []string                 `json:"cloids,omitempty"`
	CancelOIDs    []uint64                 `json:"cancel_oids,omitempty"`
	CancelCloids  []string                 `json:"cancel_cloids,omitempty"`
	BatchReceipt  json.RawMessage          `json:"batch_receipt,omitempty"`
	CancelReceipt json.RawMessage          `json:"cancel_receipt,omitempty"`
	Evidence      map[string]orderEvidence `json:"order_evidence,omitempty"`
}

// Evidence proves an order was observed, not its current live state. Keeping
// it in the original journal lets bounded recovery passes survive restarts.
type orderEvidence struct {
	Status string `json:"status"`
	OID    uint64 `json:"oid"`
	Height uint64 `json:"height"`
}

const lookupPassTimeout = 5 * time.Second

func (c *Client) journalIdentity() string {
	if c.account == "" {
		return c.SignerAddress()
	}
	return c.SignerAddress() + "-" + c.AccountAddress()
}
func (c *Client) transportIdentity() string {
	if c.WS != nil {
		return "binary-ws:" + c.WS.URL()
	}
	return "http"
}
func (c *Client) pendingPath() string {
	return filepath.Join(c.StateDir, c.journalIdentity()+".pending.json")
}
func syncDirectory(path string) error {
	f, err := os.Open(path)
	if err != nil {
		return err
	}
	defer f.Close()
	return f.Sync()
}
func (c *Client) loadPending() (*pendingRequest, error) {
	if c.account != "" {
		legacy := filepath.Join(c.StateDir, c.SignerAddress()+".pending.json")
		if _, err := os.Stat(legacy); err == nil {
			return nil, fmt.Errorf("master pending intent must be recovered before subaccount use")
		} else if !errors.Is(err, os.ErrNotExist) {
			return nil, err
		}
	}
	b, err := os.ReadFile(c.pendingPath())
	if errors.Is(err, os.ErrNotExist) {
		return nil, nil
	}
	if err != nil {
		return nil, err
	}
	var p pendingRequest
	if err = json.Unmarshal(b, &p); err != nil {
		return nil, fmt.Errorf("invalid pending journal: %w", err)
	}
	if p.Account != c.AccountAddress() || p.URL != c.URL || p.Nonce == 0 || !json.Valid(p.Action) {
		return nil, fmt.Errorf("pending journal identity/endpoint mismatch or invalid request")
	}
	if p.Signer != c.SignerAddress() || p.Transport != c.transportIdentity() {
		return nil, fmt.Errorf("pending signer/transport mismatch")
	}
	signerNonces.Lock()
	if p.Nonce > signerNonces.values[c.SignerAddress()] {
		signerNonces.values[c.SignerAddress()] = p.Nonce
	}
	signerNonces.Unlock()
	return &p, nil
}
func (c *Client) audit(event string, p *pendingRequest, detail string) error {
	f, err := os.OpenFile(filepath.Join(c.StateDir, c.journalIdentity()+".audit.jsonl"), os.O_CREATE|os.O_APPEND|os.O_WRONLY, 0600)
	if err != nil {
		return err
	}
	defer f.Close()
	if err = json.NewEncoder(f).Encode(struct {
		Time    string          `json:"time"`
		Event   string          `json:"event"`
		Request *pendingRequest `json:"request"`
		Detail  string          `json:"detail"`
	}{time.Now().UTC().Format(time.RFC3339Nano), event, p, detail}); err != nil {
		return err
	}
	return f.Sync()
}
func (c *Client) trackedExchange(ctx context.Context, action any) error {
	c.submitMu.Lock()
	defer c.submitMu.Unlock()
	if err := ctx.Err(); err != nil {
		return err
	}
	if err := os.MkdirAll(c.StateDir, 0700); err != nil {
		return err
	}
	if p, err := c.loadPending(); err != nil {
		return err
	} else if p != nil {
		return fmt.Errorf("%w (nonce=%d)", ErrPendingUnresolved, p.Nonce)
	}
	nonce, err := c.durableNonce()
	if err != nil {
		return err
	}
	p := &pendingRequest{Signer: c.SignerAddress(), Transport: c.transportIdentity(), Account: c.AccountAddress(), URL: c.URL, Nonce: nonce}
	// A caller-supplied cloid is kept (the caller may cancel or look up by it later);
	// only orders without one get a random cloid so execution can still be resolved.
	assign := func(o *Order) error {
		if o.Cloid == "" {
			var b [16]byte
			if _, err := rand.Read(b[:]); err != nil {
				return err
			}
			o.Cloid = "0x" + hex.EncodeToString(b[:])
		}
		p.Lookups = append(p.Lookups, o.Cloid)
		return nil
	}
	switch a := action.(type) {
	case Place:
		a.Orders = append([]Order(nil), a.Orders...)
		for i := range a.Orders {
			if err := assign(&a.Orders[i]); err != nil {
				return err
			}
		}
		action = a
	case *Place:
		if a == nil {
			return fmt.Errorf("nil place action")
		}
		for i := range a.Orders {
			if err := assign(&a.Orders[i]); err != nil {
				return err
			}
		}
	case Modify:
		if err := assign(&a.Order); err != nil {
			return err
		}
		p.CancelOIDs = append(p.CancelOIDs, a.OID)
		action = a
	case *Modify:
		if a == nil {
			return fmt.Errorf("nil modify action")
		}
		if err := assign(&a.Order); err != nil {
			return err
		}
		p.CancelOIDs = append(p.CancelOIDs, a.OID)
	case BatchModify:
		for i := range a.Modifies {
			p.CancelOIDs = append(p.CancelOIDs, a.Modifies[i].OID)
			if err := assign(&a.Modifies[i].Order); err != nil {
				return err
			}
		}
		action = a
	case *BatchModify:
		if a == nil {
			return fmt.Errorf("nil batch modify action")
		}
		for i := range a.Modifies {
			p.CancelOIDs = append(p.CancelOIDs, a.Modifies[i].OID)
			if err := assign(&a.Modifies[i].Order); err != nil {
				return err
			}
		}
	case Cancel:
		for _, x := range a.Cancels {
			p.CancelOIDs = append(p.CancelOIDs, x.OID)
		}
	case *Cancel:
		if a == nil {
			return fmt.Errorf("nil cancel action")
		}
		for _, x := range a.Cancels {
			p.CancelOIDs = append(p.CancelOIDs, x.OID)
		}
	case CancelByCloid:
		for _, x := range a.Cancels {
			p.CancelCloids = append(p.CancelCloids, x.Cloid.String())
		}
	case *CancelByCloid:
		if a == nil {
			return fmt.Errorf("nil cancelByCloid action")
		}
		for _, x := range a.Cancels {
			p.CancelCloids = append(p.CancelCloids, x.Cloid.String())
		}
	default:
		return fmt.Errorf("unsupported tracked action %T", action)
	}
	if len(p.Lookups)+len(p.CancelOIDs)+len(p.CancelCloids) == 0 {
		return fmt.Errorf("empty tracked action")
	}
	p.Action, err = json.Marshal(action)
	if err != nil {
		return err
	}
	f, err := os.OpenFile(c.pendingPath(), os.O_CREATE|os.O_EXCL|os.O_WRONLY, 0600)
	if err != nil {
		return err
	}
	err = json.NewEncoder(f).Encode(p)
	if err == nil {
		err = f.Sync()
	}
	closeErr := f.Close()
	if err != nil {
		return err
	}
	if closeErr != nil {
		return closeErr
	}
	if err = syncDirectory(c.StateDir); err != nil {
		return err
	}
	if err = c.audit("prepared", p, ""); err != nil {
		return err
	}
	c.logInfo("request prepared", "account", p.Account, "nonce", p.Nonce, "action", string(p.Action))
	err = c.exchangeNonce(ctx, action, p.Nonce)
	var cancellation *cancelReceipt
	if errors.As(err, &cancellation) {
		p.CancelReceipt = append(json.RawMessage(nil), cancellation.Raw...)
		if e := c.savePending(p); e != nil {
			return fmt.Errorf("pending retained: save cancel receipt: %w", e)
		}
		err = nil
	}
	var batch *batchReceipt
	if errors.As(err, &batch) {
		if e := c.saveBatchReceipt(p, batch); e != nil {
			return fmt.Errorf("pending retained: save batch receipt: %w", e)
		}
		err = nil
	}
	var rej *ActionRejectedError
	if errors.As(err, &rej) {
		// The execution layer answered with a deterministic rejection: nothing
		// ran, so the intent is final. Retire it; the next attempt gets a new nonce.
		c.LastOrderIDs, c.LastStatuses = nil, nil
		if e := c.retirePending(p, "rejected_by_execution", rej.Error()); e != nil {
			return e
		}
		c.logInfo("request rejected by execution; pending cleared", "account", p.Account, "nonce", p.Nonce, "code", rej.Code)
		out := fmt.Errorf("%w nonce=%d: %s", ErrExecutionRejected, p.Nonce, rej.Error())
		if rej.Code == CodeUnknownSigner {
			out = fmt.Errorf("%w: %w", out, ErrSignerNotRecognized)
		}
		return out
	}
	detail := "transport returned; execution unconfirmed"
	if err != nil {
		cause := err
		var unknown *UnknownOutcomeError
		if errors.As(err, &unknown) {
			cause = unknown.Cause
		}
		detail = "transport failed or ambiguous: " + cause.Error()
	}
	if e := c.audit("sent_or_uncertain", p, detail); e != nil {
		return e
	}
	c.logInfo("request sent; outcome unconfirmed", "account", p.Account, "nonce", p.Nonce, "detail", detail)
	return c.classify(p, c.resolve(ctx, p))
}

// classify turns a resolve error into an *UnknownOutcomeError unless it is a
// final rejection: whenever resolve fails with the journal retained, the
// request is still unresolved.
func (c *Client) classify(p *pendingRequest, err error) error {
	if err == nil || errors.Is(err, ErrExecutionRejected) {
		return err
	}
	cloids := append(append([]string(nil), p.Lookups...), p.CancelCloids...)
	return &UnknownOutcomeError{Nonce: p.Nonce, Account: p.Account, Action: append(json.RawMessage(nil), p.Action...), Cloids: cloids, Cause: err}
}

// RecoverPending resolves the pending request journaled in StateDir, if any.
// It only observes the old request: it queries order status and never submits,
// re-signs or replays it. It returns nil when the request is resolved or
// nothing is pending, an error wrapping ErrExecutionRejected when it resolved
// into a rejection, and an *UnknownOutcomeError while evidence is missing; call
// it again, or with a longer context. Call it at startup before readiness
// checks, before any new action and before any shutdown cancellation.
func (c *Client) RecoverPending(ctx context.Context) error {
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
	if c.StateDir == "" {
		return nil
	}
	c.submitMu.Lock()
	defer c.submitMu.Unlock()
	p, err := c.loadPending()
	if err != nil || p == nil {
		return err
	}
	c.logInfo("recovering pending request", "account", p.Account, "nonce", p.Nonce)
	return c.classify(p, c.resolve(ctx, p))
}

func (c *Client) lookup(ctx context.Context, oid any) (string, uint64, error) {
	return c.lookupState(ctx, oid, false)
}

// A legResult can prove an order was placed/filled, but cannot prove it is no
// longer live: a filled leg may have left a resting remainder. Cancellation
// reconciliation therefore requires the actual current order object.
func (c *Client) lookupState(ctx context.Context, oid any, requireOrder bool) (string, uint64, error) {
	var out struct {
		Height uint64          `json:"height"`
		Status string          `json:"status"`
		Leg    json.RawMessage `json:"leg"`
		Order  struct {
			Status string    `json:"status"`
			Order  OpenOrder `json:"order"`
		} `json:"order"`
	}
	err := c.orderQuery(ctx, map[string]any{"type": "orderStatus", "user": c.AccountAddress(), "oid": oid}, &out)
	if err != nil {
		return "", 0, err
	}
	if c.RequireExecutionHeight && out.Height == 0 {
		return "", 0, fmt.Errorf("orderStatus lacks committed height")
	}
	if c.RequireExecutionHeight && out.Height < c.LastExecutionHeight {
		return "", 0, fmt.Errorf("orderStatus height %d is behind confirmed height %d", out.Height, c.LastExecutionHeight)
	}
	if out.Status == "unknownOid" {
		return "", 0, nil
	}
	finish := func(status string, oid uint64) (string, uint64, error) {
		if err := c.observeHeight(out.Height); err != nil {
			return "", 0, err
		}
		return status, oid, nil
	}
	if out.Status == "legResult" {
		if requireOrder {
			return "", 0, fmt.Errorf("orderStatus lacks current order state")
		}
		var leg struct {
			Resting *struct {
				OID uint64 `json:"oid"`
			} `json:"resting"`
			Filled *struct {
				OID uint64 `json:"oid"`
			} `json:"filled"`
			Error string `json:"error"`
		}
		if err := json.Unmarshal(out.Leg, &leg); err != nil {
			return "", 0, fmt.Errorf("invalid legResult: %w", err)
		}
		if leg.Error != "" {
			return finish("rejected", 0)
		}
		if leg.Resting != nil && leg.Resting.OID != 0 {
			return finish("open", leg.Resting.OID)
		}
		if leg.Filled != nil && leg.Filled.OID != 0 {
			return finish("filled", leg.Filled.OID)
		}
		return "", 0, fmt.Errorf("nonterminal or invalid legResult")
	}
	if out.Status != "order" || !knownOrderState(out.Order.Status) || out.Order.Order.OID == 0 {
		return "", 0, fmt.Errorf("invalid orderStatus response")
	}
	if id, ok := oid.(uint64); ok && id != out.Order.Order.OID {
		return "", 0, fmt.Errorf("orderStatus returned a different oid")
	}
	return finish(out.Order.Status, out.Order.Order.OID)
}

func (c *Client) resolve(ctx context.Context, p *pendingRequest) error {
	c.LastOrderIDs, c.LastStatuses = nil, nil
	var cancelErrors []string
	if len(p.CancelReceipt) > 0 {
		count, err := pendingCancelCount(p)
		if err != nil {
			return err
		}
		cancelErrors, err = parseCancelReceipt(p.CancelReceipt, count)
		if err != nil {
			return err
		}
	}
	var receiptLegs []batchLeg
	if len(p.BatchReceipt) > 0 {
		var batch BatchModify
		if json.Unmarshal(p.Action, &batch) != nil || batch.Type != "batchModify" || len(batch.Modifies) != len(p.Lookups) || len(p.CancelOIDs) != len(p.Lookups) {
			return fmt.Errorf("pending retained: batch receipt/action mismatch")
		}
		for i, m := range batch.Modifies {
			if m.Order.Cloid != p.Lookups[i] || m.OID != p.CancelOIDs[i] {
				return fmt.Errorf("pending retained: batch receipt/lookup mismatch")
			}
		}
		var err error
		receiptLegs, err = parseBatchReceipt(p.BatchReceipt, len(p.Lookups))
		if err != nil {
			return fmt.Errorf("pending retained: %w", err)
		}
	}
	// Only evidence for this journal's queries can participate in recovery.
	allowed := make(map[string]bool)
	for _, cloid := range p.Lookups {
		allowed["order:"+cloid] = true
	}
	for _, oid := range p.CancelOIDs {
		allowed["cancel:"+strconv.FormatUint(oid, 10)] = true
	}
	for _, cloid := range p.CancelCloids {
		allowed["cancel-cloid:"+cloid] = true
	}
	for key, evidence := range p.Evidence {
		if !allowed[key] || !knownOrderState(evidence.Status) || evidence.Height == 0 || (evidence.OID == 0 && evidence.Status != "rejected") || (strings.HasPrefix(key, "cancel") && !inactiveOrder(evidence.Status)) {
			return fmt.Errorf("pending retained: invalid stored order evidence")
		}
		if err := c.observeHeight(evidence.Height); err != nil {
			return err
		}
	}
	if p.Evidence == nil {
		p.Evidence = make(map[string]orderEvidence)
	}
	nextLog := time.Time{}
	for {
		if err := ctx.Err(); err != nil {
			return fmt.Errorf("pending nonce=%d retained: %w", p.Nonce, err)
		}
		done := len(p.Lookups)+len(p.CancelOIDs)+len(p.CancelCloids) > 0
		// An old filled/canceled target does not establish that this cancel
		// request ended. A late cancelByCloid could affect a reused cloid.
		if len(p.Lookups) == 0 && len(cancelErrors) == 0 {
			done = false
		}
		var statuses []string
		var ids []uint64
		var states []string
		rejected := false
		lookupFailed := false
		lookupCtx, stopLookups := context.WithTimeout(ctx, lookupPassTimeout)
		dirty := false
		remember := func(key, status string, oid uint64) {
			// Only height-checked reads may become durable evidence for a
			// later recovery run.
			if c.RequireExecutionHeight {
				p.Evidence[key] = orderEvidence{Status: status, OID: oid, Height: c.LastExecutionHeight}
				dirty = true
			}
		}
		for i, cloid := range p.Lookups {
			if len(receiptLegs) > 0 && receiptLegs[i].Error != "" {
				statuses = append(statuses, fmt.Sprintf("cloid=%s rejected: %s", cloid, receiptLegs[i].Error))
				rejected = true
				continue
			}
			var key any = cloid
			if len(receiptLegs) > 0 {
				key = receiptLegs[i].OID
			}
			evidenceKey := "order:" + cloid
			evidence, known := p.Evidence[evidenceKey]
			var err error
			if !known {
				evidence.Status, evidence.OID, err = c.lookup(lookupCtx, key)
			}
			s, oid := evidence.Status, evidence.OID
			// A reused cloid can still name the old, canceled order while the
			// replacement projection lags. That is not the new order's result.
			if len(p.CancelOIDs) == len(p.Lookups) && oid == p.CancelOIDs[i] {
				s = ""
			}
			if len(receiptLegs) > 0 && oid != receiptLegs[i].OID {
				s = ""
			}
			if err != nil {
				lookupFailed = true
			}
			if err != nil || s == "" {
				done = false
			} else if !known {
				remember(evidenceKey, s, oid)
			}
			statuses = append(statuses, fmt.Sprintf("cloid=%s oid=%d status=%s", cloid, oid, s))
			if oid != 0 && s != "" {
				ids = append(ids, oid)
				states = append(states, s)
			}
			if strings.Contains(strings.ToLower(s), "reject") {
				rejected = true
			}
			if lookupCtx.Err() != nil {
				done, lookupFailed = false, true
				break
			}
		}
		for i, oid := range p.CancelOIDs {
			if len(cancelErrors) > 0 && cancelErrors[i] != "" {
				statuses = append(statuses, fmt.Sprintf("cancel oid=%d rejected: %s", oid, cancelErrors[i]))
				rejected = true
				continue
			}
			// A rejected modify leaves its old order untouched. Do not wait
			// for that order to be canceled by a failed replacement.
			if len(receiptLegs) > 0 && receiptLegs[i].Error != "" {
				continue
			}
			evidenceKey := "cancel:" + strconv.FormatUint(oid, 10)
			evidence, known := p.Evidence[evidenceKey]
			var err error
			if !known {
				evidence.Status, evidence.OID, err = c.lookupState(lookupCtx, oid, true)
			}
			s := evidence.Status
			if err != nil {
				lookupFailed = true
			}
			if err != nil || !inactiveOrder(s) {
				done = false
			} else if !known {
				remember(evidenceKey, s, evidence.OID)
			}
			statuses = append(statuses, fmt.Sprintf("oid=%d status=%s", oid, s))
			if lookupCtx.Err() != nil {
				done, lookupFailed = false, true
				break
			}
		}
		for i, cloid := range p.CancelCloids {
			if len(cancelErrors) > 0 && cancelErrors[i] != "" {
				statuses = append(statuses, fmt.Sprintf("cancel cloid=%s rejected: %s", cloid, cancelErrors[i]))
				rejected = true
				continue
			}
			evidenceKey := "cancel-cloid:" + cloid
			evidence, known := p.Evidence[evidenceKey]
			var err error
			if !known {
				evidence.Status, evidence.OID, err = c.lookupState(lookupCtx, cloid, true)
			}
			s := evidence.Status
			if err != nil {
				lookupFailed = true
			}
			if err != nil || !inactiveOrder(s) {
				done = false
			} else if !known {
				remember(evidenceKey, s, evidence.OID)
			}
			statuses = append(statuses, fmt.Sprintf("cancel_cloid=%s status=%s", cloid, s))
			if lookupCtx.Err() != nil {
				done, lookupFailed = false, true
				break
			}
		}
		stopLookups()
		if dirty {
			if err := c.savePending(p); err != nil {
				return fmt.Errorf("pending retained: save order evidence: %w", err)
			}
		}
		detail := strings.Join(statuses, "; ")
		if done {
			if err := c.retirePending(p, "execution_observed", detail); err != nil {
				return err
			}
			c.logInfo("execution observed", "account", p.Account, "nonce", p.Nonce, "detail", detail)
			c.LastOrderIDs, c.LastStatuses = ids, states
			if rejected {
				return fmt.Errorf("%w nonce=%d: %s", ErrExecutionRejected, p.Nonce, detail)
			}
			return nil
		}
		// Elapsed time and unknownOid never prove rejection. Retain the
		// journal until positive evidence resolves it or the caller cancels.
		if time.Now().After(nextLog) {
			c.logInfo("awaiting execution; no resubmission", "account", p.Account, "nonce", p.Nonce, "lookup_failed", lookupFailed)
			nextLog = time.Now().Add(30 * time.Second)
		}
		select {
		case <-ctx.Done():
		case <-time.After(500 * time.Millisecond):
		}
	}
}

func inactiveOrder(status string) bool {
	switch status {
	case "filled", "canceled", "rejected", "tickRejected", "minTradeNtlRejected",
		"reduceOnlyRejected", "badAloPxRejected", "iocCancelRejected", "perpMarginRejected",
		"oracleRejected", "insufficientSpotBalanceRejected", "selfTradeCanceled",
		"reduceOnlyCanceled", "siblingFilledCanceled", "scheduledCancel", "vaultWithdrawalCanceled":
		return true
	}
	return false
}

func knownOrderState(status string) bool {
	return status == "open" || status == "triggered" || inactiveOrder(status)
}

func (c *Client) durableNonce() (uint64, error) {
	if c.StateDir == "" {
		return c.nextNonce(), nil
	}
	path := filepath.Join(c.StateDir, c.SignerAddress()+".nonce")
	b, err := os.ReadFile(path)
	if err != nil && !errors.Is(err, os.ErrNotExist) {
		return 0, err
	}
	if len(b) > 0 {
		floor, e := strconv.ParseUint(strings.TrimSpace(string(b)), 10, 64)
		if e != nil {
			return 0, fmt.Errorf("invalid durable nonce floor: %w", e)
		}
		if floor == ^uint64(0) {
			return 0, fmt.Errorf("nonce exhausted")
		}
		signerNonces.Lock()
		if floor > signerNonces.values[c.SignerAddress()] {
			signerNonces.values[c.SignerAddress()] = floor
		}
		signerNonces.Unlock()
	}
	n := c.nextNonce()
	f, err := os.CreateTemp(c.StateDir, ".nonce-")
	if err != nil {
		return 0, err
	}
	tmp := f.Name()
	defer os.Remove(tmp)
	if _, err = f.WriteString(strconv.FormatUint(n, 10)); err == nil {
		err = f.Sync()
	}
	closeErr := f.Close()
	if err != nil {
		return 0, err
	}
	if closeErr != nil {
		return 0, closeErr
	}
	if err = os.Rename(tmp, path); err != nil {
		return 0, err
	}
	if err = syncDirectory(c.StateDir); err != nil {
		return 0, err
	}
	return n, nil
}
