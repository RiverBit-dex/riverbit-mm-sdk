package binws

import (
	"context"
	"time"
)

// Run keeps one authenticated session alive until ctx is canceled: dial, log
// in, call setup, wait until the connection ends, back off, repeat. Subscriptions
// belong to a connection, so setup must subscribe again on every connection.
//
// newConfig is called before every dial so each connection gets fresh state
// (a new Router and Book, for example). setup receives the logged-in session;
// returning an error closes it and retries. onEnd, if set, is told why a
// connection ended or could not be made, and how long Run waits before the
// next attempt.
//
// Run returns nil when ctx is canceled. Pending requests on the ended session
// fail with ErrClosed; a signed action whose reply was lost is an unknown
// outcome, to be reconciled by package exchange, never resent.
func Run(ctx context.Context, newConfig func() Config, setup func(*Session) error, onEnd func(err error, retryIn time.Duration)) error {
	backoff := time.Second
	for ctx.Err() == nil {
		err := runOnce(ctx, newConfig(), setup, &backoff)
		if ctx.Err() != nil {
			return nil
		}
		if onEnd != nil {
			onEnd(err, backoff)
		}
		select {
		case <-ctx.Done():
			return nil
		case <-time.After(backoff):
		}
		backoff = min(2*backoff, 15*time.Second)
	}
	return nil
}

func runOnce(ctx context.Context, cfg Config, setup func(*Session) error, backoff *time.Duration) error {
	sessionCtx, cancel := context.WithCancel(ctx)
	defer cancel()
	session, err := Dial(sessionCtx, cfg)
	if err != nil {
		return err
	}
	defer session.Close()
	if setup != nil {
		if err := setup(session); err != nil {
			return err
		}
	}
	*backoff = time.Second // a working connection resets the backoff
	<-session.Done()
	return session.Err()
}
