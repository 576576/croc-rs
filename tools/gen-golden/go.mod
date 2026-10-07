module github.com/croc-rs/gen-golden

go 1.27.0

require (
	github.com/schollz/croc/v11 v11.5.4
	github.com/schollz/pake/v3 v3.2.0
	github.com/tscholl2/siec v0.0.0-20240310163802-c2c6f6198406
)

require (
	filippo.io/edwards25519 v1.2.0 // indirect
	golang.org/x/crypto v0.57.0 // indirect
)

// 参考实现就在本机的 croc 检出里。pakekey 是 croc 自己加的一层，Go 模块代理上没有
// 独立版本，所以必须 replace 到本地源码——重新实现一遍来生成"参考"向量是自欺欺人。
replace github.com/schollz/croc/v11 => ../../../croc
