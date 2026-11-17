package main

import "testing"

func TestDefaultsPointAtBinWS(t *testing.T) {
	for _, k := range []string{"BIN_WS_URL", "BIN_WS_LOGIN_HOST", "API_GATEWAY_URL", "COIN", "BOOK_STREAM", "ORDER_TRANSPORT"} {
		t.Setenv(k, "")
	}
	cfg := LoadConfig()
	if cfg.BinWSURL != "wss://pre-api.riverbit.io/bin-ws" || cfg.BinWSLoginHost != "pre-api.riverbit.io" || cfg.APIGatewayURL != "https://pre-api.riverbit.io" || cfg.Coin != "BTC" || cfg.BookStream != "l2" || cfg.OrderTransport != "ws" {
		t.Fatalf("defaults = %+v", cfg)
	}
	if err := cfg.Validate(); err != nil {
		t.Fatal(err)
	}
}

func TestOrderTransport(t *testing.T) {
	for value, ok := range map[string]bool{"ws": true, "rest": true, "http": false, "WS": false} {
		t.Setenv("ORDER_TRANSPORT", value)
		cfg := LoadConfig()
		if err := cfg.Validate(); (err == nil) != ok {
			t.Errorf("ORDER_TRANSPORT=%q: Validate() = %v", value, err)
		}
	}
}
