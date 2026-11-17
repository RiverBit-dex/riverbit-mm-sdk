package exchange

import "strings"

// A throwaway key (never a real wallet): 0x00..01.
const testKeyHex = "0000000000000000000000000000000000000000000000000000000000000001"

func newTestClient(url string) (*Client, error) {
	signer, err := NewSigner(testKeyHex)
	if err != nil {
		return nil, err
	}
	return New(url, signer)
}

func newTestVaultClient(url, vault string) (*Client, error) {
	signer, err := NewSigner(strings.Repeat("0", 63) + "1")
	if err != nil {
		return nil, err
	}
	return NewForVault(url, signer, vault)
}
