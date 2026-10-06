// Command gen-golden emits wire-level known-answer vectors for croc-rs.
//
// 用法：
//
//	cd croc-rs/tools/gen-golden
//	go mod tidy          # 首次需要，生成 go.sum
//	go run . -out ../../tests/golden
//
// 产出三个文件，分别对应三个 Rust crate 的黄金向量测试：
//
//	curves.json    → crates/croc-siec/tests/golden.rs
//	pake.json      → crates/croc-pake/tests/golden.rs
//	pakekey.json   → crates/croc-pakekey/tests/golden.rs
//
// # 为什么要 replace 到本地 croc
//
// `pakekey` 是 croc 仓库里的一个包（`src/pakekey`），不是独立发布的模块。想生成
// 真正的参考向量，只能用它本人；照着 Rust 实现再写一遍 Go 版来"对照"，等于自己跟
// 自己比。见 go.mod 的 replace 指令。
//
// # 随机数怎么固定
//
// `schollz/pake` 在 `Init`/`Update` 内部调用 `crypto/rand.Read`。`crypto/rand` 的
// `Reader` 是个包级变量，所以在调用之前把它换成确定性的 `fillReader` 即可复现：
//
//	A 方 Init        → 第 1 次 Read → α = 0x11 × 32
//	B 方 Update      → 第 2 次 Read → β = 0x12 × 32
//
// 顺序是固定的（role 1 的 Init 不取随机数），所以这两个值可以直接写进向量文件。
package main

import (
	"bytes"
	"crypto/elliptic"
	"crypto/rand"
	"encoding/hex"
	"encoding/json"
	"flag"
	"fmt"
	"log"
	"math/big"
	"os"
	"path/filepath"
	"strings"

	"github.com/schollz/croc/v11/src/pakekey"
	"github.com/schollz/pake/v3"
	"github.com/tscholl2/siec"
)

const (
	vectorVersion = 1

	password  = "shared password"
	identityA = "receiver-identity"
	identityB = "sender-identity"
	room      = "room-one"

	// 盐的填充字节，和 Rust 测试里的常量保持一致。
	saltFill = 0x42
)

// 顺序与 Go 版 `pake.AvailableCurves()` 一致。
var allCurves = []string{"p521", "p256", "p384", "siec", "ed25519"}

// ed25519 不在这里：`Edwards25519Curve` 没有 `Double` 方法，凑不出和其余四条曲线
// 同构的用例。它的适配器在 croc-siec 里有单独的性质测试。
var weierstrassCurves = []string{"siec", "p256", "p384", "p521"}

func main() {
	out := flag.String("out", "../../tests/golden", "输出目录")
	flag.Parse()

	if err := os.MkdirAll(*out, 0o755); err != nil {
		log.Fatalf("mkdir %s: %v", *out, err)
	}

	writeJSON(filepath.Join(*out, "curves.json"), curveVectors())
	writeJSON(filepath.Join(*out, "pake.json"), pakeVectors())
	writeJSON(filepath.Join(*out, "pakekey.json"), pakeKeyVectors())

	fmt.Printf("黄金向量已写入 %s\n", *out)
}

func writeJSON(path string, value any) {
	encoded, err := json.MarshalIndent(value, "", "  ")
	if err != nil {
		log.Fatalf("marshal %s: %v", path, err)
	}
	encoded = append(encoded, '\n')
	if err := os.WriteFile(path, encoded, 0o644); err != nil {
		log.Fatalf("write %s: %v", path, err)
	}
	fmt.Printf("  %s\n", path)
}

// ---------------------------------------------------------------- 确定性随机源

// fillReader 让随机流完全可复现：每次 Read 用同一个填充字节，然后递增。
// 它同时记录每次读取的内容，用来事后确认随机源确实被消费了（见 expectEphemerals）。
type fillReader struct {
	fill    byte
	history [][]byte
}

func (r *fillReader) Read(p []byte) (int, error) {
	for i := range p {
		p[i] = r.fill
	}
	r.history = append(r.history, append([]byte(nil), p...))
	r.fill++
	return len(p), nil
}

// newRandom 把随机源退回起点，让每个用例都从 0x11 开始。
func newRandom() *fillReader {
	reader := &fillReader{fill: 0x11}
	rand.Reader = reader
	return reader
}

// expectEphemerals 确认随机源恰好被读两次、每次 32 字节。
//
// 这是生成器的自检：如果哪天 `crypto/rand.Read` 不再走 `rand.Reader`（或者 pake 改成
// 别的随机源），`history` 就会是空的，此时写出来的 `ephemeral_*` 字段就是假的。
// 宁可在这里炸掉，也不要产出让 Rust 侧对不上的"黄金"向量。
func expectEphemerals(reader *fillReader, context string) {
	if len(reader.history) != 2 {
		log.Fatalf(
			"%s：期望 crypto/rand 被读 2 次，实际 %d 次。说明 crypto/rand.Read 已不再走 rand.Reader，向量里的 α/β 不可信。",
			context, len(reader.history),
		)
	}
	for index, expected := range []byte{0x11, 0x12} {
		got := reader.history[index]
		if len(got) != 32 || !bytes.Equal(got, bytes.Repeat([]byte{expected}, 32)) {
			log.Fatalf("%s：第 %d 次随机读取不是 0x%02x×32", context, index+1, expected)
		}
	}
}

// ---------------------------------------------------------------- 曲线算术向量

// curveOps 抽出四条曲线共有的那部分方法。
//
// `siec.SIEC255Params.Params()` 返回 `*SIEC255Params` 而不是 `*elliptic.CurveParams`，
// 所以它不满足 `elliptic.Curve`，只能靠这个窄接口把两者统一起来。
type curveOps interface {
	IsOnCurve(x, y *big.Int) bool
	Add(x1, y1, x2, y2 *big.Int) (x, y *big.Int)
	Double(x1, y1 *big.Int) (x, y *big.Int)
	ScalarMult(x1, y1 *big.Int, k []byte) (x, y *big.Int)
	ScalarBaseMult(k []byte) (x, y *big.Int)
}

func opsFor(name string) curveOps {
	switch name {
	case "p256":
		return elliptic.P256()
	case "p384":
		return elliptic.P384()
	case "p521":
		return elliptic.P521()
	case "siec":
		return siec.SIEC255()
	}
	log.Fatalf("未知曲线 %q", name)
	return nil
}

type curveCase struct {
	Curve         string `json:"curve"`
	BaseX         string `json:"base_x"`
	BaseY         string `json:"base_y"`
	DoubledX      string `json:"doubled_x"`
	DoubledY      string `json:"doubled_y"`
	TripledX      string `json:"tripled_x"`
	TripledY      string `json:"tripled_y"`
	WideScalarHex string `json:"wide_scalar_hex"`
	WideResultX   string `json:"wide_result_x"`
	WideResultY   string `json:"wide_result_y"`
}

type curveFile struct {
	Version int         `json:"version"`
	Note    string      `json:"note"`
	Cases   []curveCase `json:"cases"`
}

func curveVectors() curveFile {
	// 32 字节的宽标量：对 NIST 曲线它比阶窄，会被 Go 的 normalizeScalar 左补零；
	// 对 siec 则是原样进 ScalarMult。两条路径都要覆盖。
	wide := bytes.Repeat([]byte{0x11}, 32)

	cases := make([]curveCase, 0, len(weierstrassCurves))
	for _, name := range weierstrassCurves {
		ops := opsFor(name)

		// 标量 1 → 基点本身。siec 走 ScalarMult(G, [1])，NIST 走 normalizeScalar
		// 之后再算，两条路径都必须落回同一个 G。
		baseX, baseY := ops.ScalarBaseMult([]byte{1})
		if !ops.IsOnCurve(baseX, baseY) {
			log.Fatalf("%s 的基点不在曲线上", name)
		}
		doubleX, doubleY := ops.Double(baseX, baseY)
		// 3G = 2G + G：把 Add 和 Double 都走一遍，互为交叉验证。
		tripleX, tripleY := ops.Add(doubleX, doubleY, baseX, baseY)
		wideX, wideY := ops.ScalarMult(baseX, baseY, wide)

		cases = append(cases, curveCase{
			Curve:         name,
			BaseX:         baseX.String(),
			BaseY:         baseY.String(),
			DoubledX:      doubleX.String(),
			DoubledY:      doubleY.String(),
			TripledX:      tripleX.String(),
			TripledY:      tripleY.String(),
			WideScalarHex: hex.EncodeToString(wide),
			WideResultX:   wideX.String(),
			WideResultY:   wideY.String(),
		})
	}

	return curveFile{
		Version: vectorVersion,
		Note: "坐标是十进制字符串（Go big.Int.String()）；标量是十六进制。",
		Cases:   cases,
	}
}

// ---------------------------------------------------------------- SPAKE2 向量

type pakeCase struct {
	Curve             string `json:"curve"`
	PasswordHex       string `json:"password_hex"`
	IdentityAHex      string `json:"identity_a_hex"`
	IdentityBHex      string `json:"identity_b_hex"`
	EphemeralAHex     string `json:"ephemeral_a_hex"`
	EphemeralBHex     string `json:"ephemeral_b_hex"`
	InitiatorValueHex string `json:"initiator_value_hex"`
	ResponderValueHex string `json:"responder_value_hex"`
	SessionKeyAHex    string `json:"session_key_a_hex"`
	SessionKeyBHex    string `json:"session_key_b_hex"`
}

type pakeFile struct {
	Version int        `json:"version"`
	Note    string     `json:"note"`
	Cases   []pakeCase `json:"cases"`
}

func pakeVectors() pakeFile {
	cases := make([]pakeCase, 0, len(allCurves))
	for _, name := range allCurves {
		reader := newRandom()

		alice, err := pake.InitCurveWithIdentities([]byte(password), 0, name, []byte(identityA), []byte(identityB))
		if err != nil {
			log.Fatalf("InitCurveWithIdentities(A, %s): %v", name, err)
		}
		initiator := append([]byte(nil), alice.Bytes()...)

		bob, err := pake.InitCurveWithIdentities([]byte(password), 1, name, []byte(identityA), []byte(identityB))
		if err != nil {
			log.Fatalf("InitCurveWithIdentities(B, %s): %v", name, err)
		}
		if err = bob.Update(initiator); err != nil {
			log.Fatalf("B.Update(%s): %v", name, err)
		}
		responder := append([]byte(nil), bob.Bytes()...)

		if err = alice.Update(responder); err != nil {
			log.Fatalf("A.Update(%s): %v", name, err)
		}
		keyA, err := alice.SessionKey()
		if err != nil {
			log.Fatalf("A.SessionKey(%s): %v", name, err)
		}
		keyB, err := bob.SessionKey()
		if err != nil {
			log.Fatalf("B.SessionKey(%s): %v", name, err)
		}
		expectEphemerals(reader, name)

		cases = append(cases, pakeCase{
			Curve:             name,
			PasswordHex:       hex.EncodeToString([]byte(password)),
			IdentityAHex:      hex.EncodeToString([]byte(identityA)),
			IdentityBHex:      hex.EncodeToString([]byte(identityB)),
			EphemeralAHex:     strings.Repeat("11", 32),
			EphemeralBHex:     strings.Repeat("12", 32),
			InitiatorValueHex: hex.EncodeToString(initiator),
			ResponderValueHex: hex.EncodeToString(responder),
			SessionKeyAHex:    hex.EncodeToString(keyA),
			SessionKeyBHex:    hex.EncodeToString(keyB),
		})
	}

	return pakeFile{
		Version: vectorVersion,
		Note: "identity 绑定版 pake v3；α = 0x11×32，β = 0x12×32。",
		Cases:   cases,
	}
}

// ---------------------------------------------------------------- pakekey 向量

type pakeKeyCase struct {
	Purpose          string `json:"purpose"`
	Room             string `json:"room"`
	Curve            string `json:"curve"`
	PasswordHex      string `json:"password_hex"`
	EphemeralAHex    string `json:"ephemeral_a_hex"`
	EphemeralBHex    string `json:"ephemeral_b_hex"`
	InitiatorHex     string `json:"initiator_hex"`
	ResponderHex     string `json:"responder_hex"`
	SaltHex          string `json:"salt_hex"`
	SharedKeyHex     string `json:"shared_key_hex"`
	EncryptionKeyHex string `json:"encryption_key_hex"`
	ConfirmationAHex string `json:"confirmation_a_hex"`
	ConfirmationBHex string `json:"confirmation_b_hex"`
}

type pakeKeyFile struct {
	Version int           `json:"version"`
	Note    string        `json:"note"`
	Cases   []pakeKeyCase `json:"cases"`
}

func pakeKeyVectors() pakeKeyFile {
	purposes := []string{pakekey.PurposeTransfer, pakekey.PurposeLocalProbe, pakekey.PurposeSSH}
	salt := bytes.Repeat([]byte{saltFill}, pakekey.SaltSize)

	cases := make([]pakeKeyCase, 0, len(allCurves)*len(purposes))
	for _, name := range allCurves {
		for _, purpose := range purposes {
			reader := newRandom()

			alice, err := pakekey.Init([]byte(password), 0, name, purpose, room)
			if err != nil {
				log.Fatalf("pakekey.Init(A, %s, %s): %v", name, purpose, err)
			}
			initiator := append([]byte(nil), alice.Bytes()...)

			bob, err := pakekey.Init([]byte(password), 1, name, purpose, room)
			if err != nil {
				log.Fatalf("pakekey.Init(B, %s, %s): %v", name, purpose, err)
			}
			if err = bob.Update(initiator); err != nil {
				log.Fatalf("B.Update(%s, %s): %v", name, purpose, err)
			}
			responder := append([]byte(nil), bob.Bytes()...)

			if err = alice.Update(responder); err != nil {
				log.Fatalf("A.Update(%s, %s): %v", name, purpose, err)
			}
			sharedKey, err := alice.SessionKey()
			if err != nil {
				log.Fatalf("A.SessionKey(%s, %s): %v", name, purpose, err)
			}
			expectEphemerals(reader, name+"/"+purpose)

			keys, err := pakekey.Derive(sharedKey, pakekey.Context{
				Purpose:   purpose,
				Room:      room,
				Curve:     name,
				Initiator: initiator,
				Responder: responder,
				Salt:      salt,
			})
			if err != nil {
				log.Fatalf("pakekey.Derive(%s, %s): %v", name, purpose, err)
			}

			cases = append(cases, pakeKeyCase{
				Purpose:          purpose,
				Room:             room,
				Curve:            name,
				PasswordHex:      hex.EncodeToString([]byte(password)),
				EphemeralAHex:    strings.Repeat("11", 32),
				EphemeralBHex:    strings.Repeat("12", 32),
				InitiatorHex:     hex.EncodeToString(initiator),
				ResponderHex:     hex.EncodeToString(responder),
				SaltHex:          hex.EncodeToString(salt),
				SharedKeyHex:     hex.EncodeToString(sharedKey),
				EncryptionKeyHex: hex.EncodeToString(keys.EncryptionKey),
				ConfirmationAHex: hex.EncodeToString(keys.ConfirmationA),
				ConfirmationBHex: hex.EncodeToString(keys.ConfirmationB),
			})
		}
	}

	return pakeKeyFile{
		Version: vectorVersion,
		Note:    "身份绑定 + HKDF-SHA256 的完整密钥编排；身份由 purpose/room 推出，不在文件里重复。",
		Cases:   cases,
	}
}
