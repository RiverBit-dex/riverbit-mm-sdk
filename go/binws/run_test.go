package binws_test

import (
	"context"
	"sync/atomic"
	"testing"
	"time"

	"github.com/riverbit-dex/riverbit-mm-sdk/go/binws"
	"github.com/riverbit-dex/riverbit-mm-sdk/go/binws/binwstest"
)

func TestRunReconnectsRelogsAndResubscribes(t *testing.T) {
	sign, _ := signer(t)
	server := binwstest.New(t, "pre-api.riverbit.io")
	var setups atomic.Int32
	ctx, cancel := context.WithCancel(context.Background())
	defer cancel()
	done := make(chan error, 1)
	ended := make(chan error, 4)
	go func() {
		done <- binws.Run(ctx, func() binws.Config {
			return binws.Config{URL: server.URL(), LoginHost: "pre-api.riverbit.io", Sign: sign}
		}, func(s *binws.Session) error {
			setups.Add(1)
			return s.Subscribe(binws.L2("RBT"))
		}, func(err error, _ time.Duration) { ended <- err })
	}()
	waitFor := func(what string, ok func() bool) {
		t.Helper()
		deadline := time.Now().Add(10 * time.Second)
		for !ok() {
			if time.Now().After(deadline) {
				t.Fatalf("timed out waiting for %s", what)
			}
			time.Sleep(10 * time.Millisecond)
		}
	}
	waitFor("the first login", func() bool { return len(server.Logins()) >= 1 && setups.Load() >= 1 })
	server.CloseConnections()
	select {
	case <-ended:
	case <-time.After(10 * time.Second):
		t.Fatal("Run did not notice the dropped connection")
	}
	waitFor("the second login and subscription", func() bool { return len(server.Logins()) >= 2 && setups.Load() >= 2 })
	cancel()
	if err := <-done; err != nil {
		t.Fatal(err)
	}
}
