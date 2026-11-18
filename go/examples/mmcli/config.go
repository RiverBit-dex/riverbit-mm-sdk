package main

import (
	"fmt"
	"os"
	"strings"
)

// Config is read from environment variables; see LoadConfig.
type Config struct {
	// BinWSURL is the bin-ws endpoint used for market data and order events
	// (one long-lived connection per process).
	BinWSURL string
	// BinWSLoginHost is signed into the login message ("host:" line). It must
	// equal the server's configured public host.
	BinWSLoginHost string
	// APIGatewayURL is the HTTP base; orders POST to <base>/exchange and
	// queries POST to <base>/info.
	APIGatewayURL string
	// Coin is the market the example watches and trades, for example "BTC".
	Coin string
	// BookStream selects the order book subscription: "l2" (incremental, the
	// default) or "l2Book" (full snapshots).
	BookStream string
	// GuildVault, if set, makes the trade command act for this vault. The
	// signer must be the vault leader or the leader's authorized API wallet.
	GuildVault string
	// KeyFile holds the private key of the API wallet or master account.
	KeyFile string
	// StateDir is the durable pending journal directory of the signer.
	StateDir string
	// SignChainID, if set, must equal the chainIdentity answer.
	SignChainID string
}

// Defaults.
const (
	DefaultBinWSURL       = "wss://pre-api.riverbit.io/bin-ws"
	DefaultBinWSLoginHost = "pre-api.riverbit.io"
	DefaultAPIGatewayURL  = "https://pre-api.riverbit.io"
)

// LoadConfig reads the example's settings from environment variables.
func LoadConfig() Config {
	cfg := Config{
		BinWSURL:       value("BIN_WS_URL", DefaultBinWSURL),
		BinWSLoginHost: value("BIN_WS_LOGIN_HOST", DefaultBinWSLoginHost),
		APIGatewayURL:  value("API_GATEWAY_URL", DefaultAPIGatewayURL),
		Coin:           value("COIN", "BTC"),
		BookStream:     value("BOOK_STREAM", "l2"),
		GuildVault:     strings.ToLower(os.Getenv("GUILD_VAULT_ADDRESS")),
		KeyFile:        value("KEY_FILE", "key"),
		StateDir:       value("STATE_DIR", "state"),
		SignChainID:    os.Getenv("SIGN_CHAIN_ID"),
	}
	return cfg
}

// Validate checks values that would otherwise fail late on the wire.
func (c Config) Validate() error {
	if c.BookStream != "l2" && c.BookStream != "l2Book" {
		return fmt.Errorf("BOOK_STREAM must be l2 or l2Book, got %q", c.BookStream)
	}
	if c.BinWSLoginHost == "" {
		return fmt.Errorf("BIN_WS_LOGIN_HOST is empty")
	}
	return nil
}

func value(key, fallback string) string {
	if v := os.Getenv(key); v != "" {
		return v
	}
	return fallback
}
