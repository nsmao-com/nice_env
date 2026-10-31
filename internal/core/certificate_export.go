package core

import (
	"bytes"
	"crypto"
	"crypto/aes"
	"crypto/cipher"
	"crypto/hmac"
	"crypto/pbkdf2"
	"crypto/rand"
	"crypto/sha1"
	"crypto/sha256"
	"crypto/x509"
	"crypto/x509/pkix"
	"encoding/asn1"
	"encoding/binary"
	"strings"
	"time"
	"unicode/utf16"
	"unicode/utf8"
)

var oidData = asn1.ObjectIdentifier{1, 2, 840, 113549, 1, 7, 1}
var oidEncryptedData = asn1.ObjectIdentifier{1, 2, 840, 113549, 1, 7, 6}

type pfxContent struct {
	Type  asn1.ObjectIdentifier
	Value asn1.RawValue
}
type pfxBag struct {
	Type       asn1.ObjectIdentifier
	Value      asn1.RawValue
	Attributes []pfxAttribute `asn1:"set,optional"`
}
type pfxAttribute struct {
	Type   asn1.ObjectIdentifier
	Values []asn1.RawValue `asn1:"set"`
}
type encryptedPrivateKey struct {
	Algorithm pkix.AlgorithmIdentifier
	Data      []byte
}

func explicitDER(der []byte) asn1.RawValue {
	return asn1.RawValue{Class: 2, Tag: 0, IsCompound: true, Bytes: der}
}
func javaPassword(password string, terminated bool) []byte {
	units := utf16.Encode([]rune(password))
	if terminated {
		units = append(units, 0)
	}
	out := make([]byte, len(units)*2)
	for i, unit := range units {
		binary.BigEndian.PutUint16(out[i*2:], unit)
	}
	return out
}

// PBES2 uses UTF-8 passwords, PBKDF2-HMAC-SHA256 and AES-256-CBC.
// Keep the format compatible with the previous release and OpenSSL 3.
func encryptPFX(data []byte, password string) (encryptedPrivateKey, error) {
	salt := make([]byte, 32)
	iv := make([]byte, aes.BlockSize)
	if _, err := rand.Read(salt); err != nil {
		return encryptedPrivateKey{}, err
	}
	if _, err := rand.Read(iv); err != nil {
		return encryptedPrivateKey{}, err
	}
	key, err := pbkdf2.Key(sha256.New, password, salt, 10000, 32)
	if err != nil {
		return encryptedPrivateKey{}, err
	}
	defer clear(key)
	block, err := aes.NewCipher(key)
	if err != nil {
		return encryptedPrivateKey{}, err
	}
	padding := aes.BlockSize - len(data)%aes.BlockSize
	plain := append(append([]byte{}, data...), bytes.Repeat([]byte{byte(padding)}, padding)...)
	defer clear(plain)
	encrypted := make([]byte, len(plain))
	cipher.NewCBCEncrypter(block, iv).CryptBlocks(encrypted, plain)
	kdf, err := asn1.Marshal(struct {
		Salt       []byte
		Iterations int
		PRF        pkix.AlgorithmIdentifier
	}{salt, 10000, pkix.AlgorithmIdentifier{Algorithm: asn1.ObjectIdentifier{1, 2, 840, 113549, 2, 9}, Parameters: asn1.NullRawValue}})
	if err != nil {
		return encryptedPrivateKey{}, err
	}
	ivDER, err := asn1.Marshal(iv)
	if err != nil {
		return encryptedPrivateKey{}, err
	}
	params, err := asn1.Marshal(struct{ KDF, Encryption pkix.AlgorithmIdentifier }{pkix.AlgorithmIdentifier{Algorithm: asn1.ObjectIdentifier{1, 2, 840, 113549, 1, 5, 12}, Parameters: asn1.RawValue{FullBytes: kdf}}, pkix.AlgorithmIdentifier{Algorithm: asn1.ObjectIdentifier{2, 16, 840, 1, 101, 3, 4, 1, 42}, Parameters: asn1.RawValue{FullBytes: ivDER}}})
	if err != nil {
		return encryptedPrivateKey{}, err
	}
	return encryptedPrivateKey{pkix.AlgorithmIdentifier{Algorithm: asn1.ObjectIdentifier{1, 2, 840, 113549, 1, 5, 13}, Parameters: asn1.RawValue{FullBytes: params}}, encrypted}, nil
}

func buildPFX(chain []*x509.Certificate, key crypto.Signer, password string) ([]byte, error) {
	if strings.ContainsRune(password, 0) || len(password) > 4096 {
		return nil, invalid("PFX 密码不能包含 NUL，且不能超过 4096 字节")
	}
	keyDER, err := x509.MarshalPKCS8PrivateKey(key)
	if err != nil {
		return nil, err
	}
	defer clear(keyDER)
	id := sha256.Sum256(chain[0].Raw)
	idDER, err := asn1.Marshal(id[:])
	if err != nil {
		return nil, err
	}
	attributes := []pfxAttribute{{asn1.ObjectIdentifier{1, 2, 840, 113549, 1, 9, 21}, []asn1.RawValue{{FullBytes: idDER}}}}
	bags := []pfxBag{}
	for i, cert := range chain {
		value, err := asn1.Marshal(cert.Raw)
		if err != nil {
			return nil, err
		}
		bag, err := asn1.Marshal(struct {
			Type  asn1.ObjectIdentifier
			Value asn1.RawValue
		}{asn1.ObjectIdentifier{1, 2, 840, 113549, 1, 9, 22, 1}, explicitDER(value)})
		if err != nil {
			return nil, err
		}
		entry := pfxBag{Type: asn1.ObjectIdentifier{1, 2, 840, 113549, 1, 12, 10, 1, 3}, Value: explicitDER(bag)}
		if i == 0 {
			entry.Attributes = attributes
		}
		bags = append(bags, entry)
	}
	certDER, err := asn1.Marshal(bags)
	if err != nil {
		return nil, err
	}
	encryptedCerts, err := encryptPFX(certDER, password)
	if err != nil {
		return nil, err
	}
	certSafe, err := asn1.Marshal(struct {
		Version int
		Info    struct {
			Type      asn1.ObjectIdentifier
			Algorithm pkix.AlgorithmIdentifier
			Data      asn1.RawValue
		}
	}{Version: 0, Info: struct {
		Type      asn1.ObjectIdentifier
		Algorithm pkix.AlgorithmIdentifier
		Data      asn1.RawValue
	}{oidData, encryptedCerts.Algorithm, asn1.RawValue{Class: 2, Tag: 0, Bytes: encryptedCerts.Data}}})
	if err != nil {
		return nil, err
	}
	encryptedKey, err := encryptPFX(keyDER, password)
	if err != nil {
		return nil, err
	}
	encryptedKeyDER, err := asn1.Marshal(encryptedKey)
	if err != nil {
		return nil, err
	}
	keySafe, err := asn1.Marshal([]pfxBag{{Type: asn1.ObjectIdentifier{1, 2, 840, 113549, 1, 12, 10, 1, 2}, Value: explicitDER(encryptedKeyDER), Attributes: attributes}})
	if err != nil {
		return nil, err
	}
	keyOctets, err := asn1.Marshal(keySafe)
	if err != nil {
		return nil, err
	}
	safes, err := asn1.Marshal([]pfxContent{{oidEncryptedData, explicitDER(certSafe)}, {oidData, explicitDER(keyOctets)}})
	if err != nil {
		return nil, err
	}
	// RFC 7292 appendix B: only one SHA-256 output block is needed for the MAC
	// key. D is the MAC diversifier; S/P repeat to multiples of the hash block.
	salt := make([]byte, 32)
	if _, err = rand.Read(salt); err != nil {
		return nil, err
	}
	passwordBytes := javaPassword(password, true)
	defer clear(passwordBytes)
	repeat := func(data []byte) []byte {
		n := 64 * ((len(data) + 63) / 64)
		out := make([]byte, n)
		for i := range out {
			out[i] = data[i%len(data)]
		}
		return out
	}
	input := append(bytes.Repeat([]byte{3}, 64), repeat(salt)...)
	input = append(input, repeat(passwordBytes)...)
	defer clear(input)
	derived := sha256.Sum256(input)
	for i := 1; i < 10000; i++ {
		derived = sha256.Sum256(derived[:])
	}
	defer clear(derived[:])
	mac := hmac.New(sha256.New, derived[:])
	mac.Write(safes)
	octets, err := asn1.Marshal(safes)
	if err != nil {
		return nil, err
	}
	type digestInfo struct {
		Algorithm pkix.AlgorithmIdentifier
		Digest    []byte
	}
	type macData struct {
		Digest     digestInfo
		Salt       []byte
		Iterations int
	}
	return asn1.Marshal(struct {
		Version int
		Safe    pfxContent
		MAC     macData
	}{3, pfxContent{oidData, explicitDER(octets)}, macData{digestInfo{pkix.AlgorithmIdentifier{Algorithm: asn1.ObjectIdentifier{2, 16, 840, 1, 101, 3, 4, 2, 1}, Parameters: asn1.NullRawValue}, mac.Sum(nil)}, salt, 10000}})
}

// JKS is a legacy format: its mandated Sun key protector and store integrity
// use SHA-1. PFX is the stronger alternative; the format must match Java exactly.
func buildJKS(chain []*x509.Certificate, key crypto.Signer, password, alias string) ([]byte, error) {
	if utf8.RuneCountInString(password) < 6 || len(password) > 4096 {
		return nil, invalid("JKS 密码至少 6 个字符且不超过 4096 字节")
	}
	keyDER, err := x509.MarshalPKCS8PrivateKey(key)
	if err != nil {
		return nil, err
	}
	defer clear(keyDER)
	pass := javaPassword(password, false)
	defer clear(pass)
	salt := make([]byte, 20)
	if _, err = rand.Read(salt); err != nil {
		return nil, err
	}
	encrypted := make([]byte, len(keyDER))
	previous := salt
	for offset := 0; offset < len(keyDER); offset += sha1.Size {
		hash := sha1.New()
		hash.Write(pass)
		hash.Write(previous)
		previous = hash.Sum(nil)
		for i := 0; i < sha1.Size && offset+i < len(keyDER); i++ {
			encrypted[offset+i] = keyDER[offset+i] ^ previous[i]
		}
	}
	hash := sha1.New()
	hash.Write(pass)
	hash.Write(keyDER)
	protected := append(append(append([]byte{}, salt...), encrypted...), hash.Sum(nil)...)
	protectedDER, err := asn1.Marshal(encryptedPrivateKey{pkix.AlgorithmIdentifier{Algorithm: asn1.ObjectIdentifier{1, 3, 6, 1, 4, 1, 42, 2, 17, 1, 1}, Parameters: asn1.NullRawValue}, protected})
	if err != nil {
		return nil, err
	}
	var out bytes.Buffer
	u32 := func(v uint32) { var b [4]byte; binary.BigEndian.PutUint32(b[:], v); out.Write(b[:]) }
	u64 := func(v uint64) { var b [8]byte; binary.BigEndian.PutUint64(b[:], v); out.Write(b[:]) }
	text := func(value string) error {
		var encoded bytes.Buffer
		for _, unit := range utf16.Encode([]rune(value)) {
			switch {
			case unit >= 1 && unit <= 0x7f:
				encoded.WriteByte(byte(unit))
			case unit <= 0x7ff:
				encoded.WriteByte(byte(0xc0 | unit>>6))
				encoded.WriteByte(byte(0x80 | unit&0x3f))
			default:
				encoded.WriteByte(byte(0xe0 | unit>>12))
				encoded.WriteByte(byte(0x80 | (unit>>6)&0x3f))
				encoded.WriteByte(byte(0x80 | unit&0x3f))
			}
		}
		if encoded.Len() > 65535 {
			return invalid("JKS 别名过长")
		}
		var length [2]byte
		binary.BigEndian.PutUint16(length[:], uint16(encoded.Len()))
		out.Write(length[:])
		out.Write(encoded.Bytes())
		return nil
	}
	u32(0xfeedfeed)
	u32(2)
	u32(1)
	u32(1)
	if err = text(strings.ToLower(alias)); err != nil {
		return nil, err
	}
	u64(uint64(time.Now().UnixMilli()))
	u32(uint32(len(protectedDER)))
	out.Write(protectedDER)
	u32(uint32(len(chain)))
	for _, cert := range chain {
		if err = text("X.509"); err != nil {
			return nil, err
		}
		u32(uint32(len(cert.Raw)))
		out.Write(cert.Raw)
	}
	integrity := sha1.New()
	integrity.Write(pass)
	integrity.Write([]byte("Mighty Aphrodite"))
	integrity.Write(out.Bytes())
	out.Write(integrity.Sum(nil))
	return out.Bytes(), nil
}
