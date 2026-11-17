package exchange

import (
	"crypto/ecdsa"
	"fmt"
	"math/big"
	"strings"

	"github.com/ethereum/go-ethereum/crypto"
	"github.com/riverbit-dex/riverbit-mm-sdk/go/binws"
)

// Signature is the {r, s, v} signature object of a trading request. R and S
// are 0x-prefixed hexadecimal without leading zeroes, V is 27 or 28.
type Signature struct {
	R string `json:"r"`
	S string `json:"s"`
	V byte   `json:"v"`
}

// Signer holds the private key of an API wallet or master account. It signs
// bin-ws logins (EIP-191) and, through a Client, trading actions (EIP-712).
// The key never leaves the Signer and is never printed.
type Signer struct {
	key *ecdsa.PrivateKey
}

var _ binws.Signer = (*Signer)(nil)

// NewSigner parses a 32-byte secp256k1 private key in hexadecimal, with or
// without a 0x prefix.
func NewSigner(hexKey string) (*Signer, error) {
	key, err := crypto.HexToECDSA(strings.TrimPrefix(strings.TrimSpace(hexKey), "0x"))
	if err != nil {
		return nil, fmt.Errorf("invalid private key")
	}
	return &Signer{key: key}, nil
}

// Address returns the signer's lowercase 0x-prefixed address.
func (s *Signer) Address() string {
	return strings.ToLower(crypto.PubkeyToAddress(s.key.PublicKey).Hex())
}

// PersonalSign signs msg with EIP-191 personal_sign and returns 65 bytes
// r || s || v with v = 27 or 28. It implements binws.Signer, so a *Signer can
// be passed as binws.Config.Sign.
func (s *Signer) PersonalSign(msg []byte) ([]byte, error) {
	sig, err := crypto.Sign(binws.PersonalHash(msg), s.key)
	if err != nil {
		return nil, err
	}
	sig[64] += 27
	return sig, nil
}

func (s *Signer) signDigest(digest []byte) (Signature, error) {
	sig, err := crypto.Sign(digest, s.key)
	if err != nil {
		return Signature{}, err
	}
	return Signature{
		R: fmt.Sprintf("0x%x", new(big.Int).SetBytes(sig[:32])),
		S: fmt.Sprintf("0x%x", new(big.Int).SetBytes(sig[32:64])),
		V: sig[64] + 27,
	}, nil
}

// signatureBytes converts a Signature to 65 bytes r || s || v with v = 0/1,
// the form crypto.SigToPub expects.
func signatureBytes(s Signature) ([]byte, error) {
	r, ok := new(big.Int).SetString(strings.TrimPrefix(s.R, "0x"), 16)
	if !ok || r.Sign() <= 0 || r.BitLen() > 256 {
		return nil, fmt.Errorf("invalid signature r")
	}
	v, ok := new(big.Int).SetString(strings.TrimPrefix(s.S, "0x"), 16)
	if !ok || v.Sign() <= 0 || v.BitLen() > 256 {
		return nil, fmt.Errorf("invalid signature s")
	}
	if s.V != 27 && s.V != 28 {
		return nil, fmt.Errorf("invalid signature v")
	}
	out := make([]byte, crypto.SignatureLength)
	r.FillBytes(out[:32])
	v.FillBytes(out[32:64])
	out[64] = s.V - 27
	return out, nil
}
