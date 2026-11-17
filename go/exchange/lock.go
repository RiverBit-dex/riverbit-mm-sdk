package exchange

import (
	"fmt"
	"os"
	"path/filepath"
)

// lockDurableSigner takes the cross-process lock of the signer's StateDir. The
// canonical StateDir must be shared by every process using this signer.
// Nonblocking ownership prevents a second process from sending while recovery
// is unresolved. The caller is responsible for canonical directory selection.
func (c *Client) lockDurableSigner() (func(), error) {
	if c.StateDir == "" {
		if c.account != "" {
			return nil, fmt.Errorf("vault or subaccount client requires a durable StateDir")
		}
		return func() {}, nil
	}
	if err := os.MkdirAll(c.StateDir, 0700); err != nil {
		return nil, err
	}
	f, err := os.OpenFile(filepath.Join(c.StateDir, c.SignerAddress()+".signer.lock"), os.O_CREATE|os.O_RDWR, 0600)
	if err != nil {
		return nil, err
	}
	if err = lockFile(f); err != nil {
		f.Close()
		return nil, fmt.Errorf("signer is owned by another process: %w", err)
	}
	return func() { unlockFile(f); f.Close() }, nil
}
