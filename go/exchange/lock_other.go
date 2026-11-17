//go:build !unix

package exchange

import (
	"errors"
	"os"
)

// The durable pending journal needs an advisory file lock, which this
// platform does not provide here. Use a Client without StateDir, or run on Linux or macOS.
func lockFile(*os.File) error {
	return errors.New("durable StateDir is not supported on this platform")
}
func unlockFile(*os.File) {}
