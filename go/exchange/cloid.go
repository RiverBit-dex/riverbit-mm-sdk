package exchange

import (
	"encoding/hex"
	"encoding/json"
	"fmt"
	"strings"

	"github.com/vmihailenco/msgpack/v5"
)

// Cloid is a 128-bit client order ID. JSON and MessagePack both carry the
// 0x-prefixed lowercase hex string, which is what the node signs: encoding it
// as 16 binary bytes would recover a different signer (code 91).
type Cloid [16]byte

// String returns the 0x-prefixed lowercase hex form.
func (c Cloid) String() string { return "0x" + hex.EncodeToString(c[:]) }

// MarshalJSON encodes the cloid as its 0x-prefixed hex string.
func (c Cloid) MarshalJSON() ([]byte, error) { return json.Marshal(c.String()) }

// UnmarshalJSON decodes a 0x-prefixed 128-bit hex string and rejects any other form.
func (c *Cloid) UnmarshalJSON(data []byte) error {
	var value string
	if err := json.Unmarshal(data, &value); err != nil {
		return fmt.Errorf("cloid must be a 0x-prefixed 128-bit hex string")
	}
	if !strings.HasPrefix(value, "0x") || len(value) != 34 {
		return fmt.Errorf("cloid must be a 0x-prefixed 128-bit hex string")
	}
	decoded, err := hex.DecodeString(value[2:])
	if err != nil || len(decoded) != len(c) {
		return fmt.Errorf("cloid must be a 0x-prefixed 128-bit hex string")
	}
	copy(c[:], decoded)
	return nil
}

// MarshalMsgpack encodes the same 0x string as JSON. The node re-encodes
// request JSON as MessagePack to verify the signature, so the cloid must be a
// string in both representations.
func (c Cloid) MarshalMsgpack() ([]byte, error) {
	return msgpack.Marshal(c.String())
}
