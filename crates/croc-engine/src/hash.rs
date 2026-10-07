//! XXH64：croc 的默认文件哈希算法。
//!
//! Go 侧是 `github.com/cespare/xxhash/v2`，`Sum(nil)` 给出 **8 字节大端**的
//! `Sum64()`。croc 用这个哈希做两件事：
//!
//! 1. 接收方拿它判断"本地已有的同名文件是不是就是这份"，一致就整个跳过；
//! 2. `--hash md5` 等其它算法是同一位置的替换项。
//!
//! 哈希**不一致只是导致重传**，不会导致数据损坏——所以就算算错了也不会传坏文件。
//! 但它是"已存在就跳过"的依据，算错了会让本该跳过的文件白传一遍，互通时也难看。
//! 所以这里按官方参考实现逐常数复刻，并用已知向量钉住。
//!
//! 为什么要自己写而不是拉一个 crate：本项目的核心 crate 依赖清单是受门禁保护的
//! （`cargo xtask deps`），而 XXH64 只有几十行、规格是公开且冻结的。少一个依赖，
//! Android 交叉编译就少一个变量。
//!
//! 另外三种算法（`md5` / `imohash` / `highway`）**用 crate**：它们要么有大量边界
//! 情况（MD5 的填充）、要么带几十个常数（HighwayHash 的置换表），自己复刻的出错面
//! 比引入一个纯 Rust 依赖大得多。

use std::fs;
use std::path::Path;

use highway::HighwayHash as _;
use md5::Digest as _;

use crate::error::EngineError;

/// 质数常数，全部来自 XXH64 规格。
const PRIME64_1: u64 = 0x9E37_79B1_85EB_CA87;
const PRIME64_2: u64 = 0xC2B2_AE3D_27D4_EB4F;
const PRIME64_3: u64 = 0x1656_67B1_9E37_79F9;
const PRIME64_4: u64 = 0x85EB_CA77_C2B2_AE63;
const PRIME64_5: u64 = 0x27D4_EB2F_1656_67C5;

/// 累加器一次吃 32 字节。
const STRIPE: usize = 32;

/// 计算一段字节的 XXH64。
#[must_use]
pub fn xxh64(data: &[u8]) -> u64 {
    xxh64_seeded(data, 0)
}

/// 带种子的 XXH64。croc 用的是种子 0，但把种子做成参数能让测试用官方向量交叉验证。
///
/// 结构严格照官方规格：32 字节一条的累加器 → 尾部按 8/4/1 字节阶梯吃掉 → 雪崩。
/// 尾部那三段最容易漏，漏了的话"长度不是 32 的倍数"的输入全都会算错。
#[must_use]
pub fn xxh64_seeded(data: &[u8], seed: u64) -> u64 {
    let mut offset = 0usize;
    let mut hash;

    if data.len() >= STRIPE {
        let mut acc = [
            seed.wrapping_add(PRIME64_1).wrapping_add(PRIME64_2),
            seed.wrapping_add(PRIME64_2),
            seed,
            seed.wrapping_sub(PRIME64_1),
        ];
        let (stripes, tail) = data.as_chunks::<STRIPE>();
        for stripe in stripes {
            let (lanes, _) = stripe.as_chunks::<8>();
            for (index, lane) in lanes.iter().enumerate() {
                if let Some(slot) = acc.get_mut(index) {
                    *slot = round(*slot, u64::from_le_bytes(*lane));
                }
            }
        }
        offset = data.len() - tail.len();
        hash = acc
            .first()
            .copied()
            .unwrap_or(0)
            .rotate_left(1)
            .wrapping_add(acc.get(1).copied().unwrap_or(0).rotate_left(7))
            .wrapping_add(acc.get(2).copied().unwrap_or(0).rotate_left(12))
            .wrapping_add(acc.get(3).copied().unwrap_or(0).rotate_left(18));
        hash = merge_round(hash, acc.first().copied().unwrap_or(0));
        hash = merge_round(hash, acc.get(1).copied().unwrap_or(0));
        hash = merge_round(hash, acc.get(2).copied().unwrap_or(0));
        hash = merge_round(hash, acc.get(3).copied().unwrap_or(0));
    } else {
        hash = seed.wrapping_add(PRIME64_5);
    }

    hash = hash.wrapping_add(data.len() as u64);

    // 尾部阶梯：8 字节 → 4 字节 → 1 字节。
    let tail = data.get(offset..).unwrap_or_default();
    let mut tail = tail;
    while let Some((head, rest)) = tail.split_at_checked(8) {
        let word = u64::from_le_bytes(head.try_into().unwrap_or_default());
        hash ^= round(0, word);
        hash = hash.rotate_left(27).wrapping_mul(PRIME64_1).wrapping_add(PRIME64_4);
        tail = rest;
    }
    if let Some((head, rest)) = tail.split_at_checked(4) {
        let word = u32::from_le_bytes(head.try_into().unwrap_or_default());
        hash ^= u64::from(word).wrapping_mul(PRIME64_1);
        hash = hash.rotate_left(23).wrapping_mul(PRIME64_2).wrapping_add(PRIME64_3);
        tail = rest;
    }
    for byte in tail {
        hash ^= u64::from(*byte).wrapping_mul(PRIME64_5);
        hash = hash.rotate_left(11).wrapping_mul(PRIME64_1);
    }

    avalanche(hash)
}

/// 一轮累加。
fn round(acc: u64, input: u64) -> u64 {
    acc.wrapping_add(input.wrapping_mul(PRIME64_2)).rotate_left(31).wrapping_mul(PRIME64_1)
}

/// 把一条累加通道并进最终值。
fn merge_round(acc: u64, lane: u64) -> u64 {
    (acc ^ round(0, lane)).wrapping_mul(PRIME64_1).wrapping_add(PRIME64_4)
}

/// 雪崩。
fn avalanche(mut hash: u64) -> u64 {
    hash ^= hash >> 33;
    hash = hash.wrapping_mul(PRIME64_2);
    hash ^= hash >> 29;
    hash = hash.wrapping_mul(PRIME64_3);
    hash ^= hash >> 32;
    hash
}

/// croc 上线路的哈希形态：8 字节**大端**。
///
/// Go 的 `xxhash.Sum(nil)` 就是这个编码，接收方拿它和自己的计算结果比。
#[must_use]
pub fn xxh64_bytes(data: &[u8]) -> Vec<u8> {
    xxh64(data).to_be_bytes().to_vec()
}

/// `--hash` 能给的四种算法名（空串与 `xxhash` 等价，都是默认）。
///
/// 与 Go 的 `progressive_hash.go:24` 一致。少了 `imohash-v2`：那是 Go 的"渐进式哈希"
/// 协商项（`progressive-file-hash-v1`），需要额外的往返消息，本实现没有谈这条能力，
/// 所以也不接受这个名字。
pub const ALGORITHMS: [&str; 4] = ["xxhash", "md5", "imohash", "highway"];

/// HighwayHash 的密钥。Go 把它硬编码在 `utils/utils.go:237`，是个固定的 32 字节值——
/// 两端必须用同一个 key，否则算出来的摘要对不上。
const HIGHWAY_KEY: [u8; 32] = [
    0x15, 0x53, 0xc5, 0x38, 0x3f, 0xb0, 0xb8, 0x65, 0x78, 0xc3, 0x31, 0x0d, 0xa6, 0x65, 0xb4, 0xf6, 0xe0, 0x52, 0x1a,
    0xcf, 0x22, 0xeb, 0x58, 0xa9, 0x95, 0x32, 0xff, 0xed, 0x02, 0xa6, 0xb1, 0x15,
];

/// imohash 的采样参数，来自 Go 的 `imohash.NewCustom(16*16*8*1024, 128*1024)`
/// （`utils/utils.go:286`）——**不是** imohash 的默认值（16 KiB / 128 KiB）。
///
/// Go 把采样窗口放大到 2 MiB，是为了让"同名但内容不同"的大文件仍能被区分开。
/// 用默认值算出来的摘要与 Go 不通，而 imohash 的摘要长度固定 16 字节，看起来
/// 完全正常，只有对端报"哈希不匹配"才会暴露。
const IMO_SAMPLE_SIZE: u32 = 16 * 16 * 8 * 1024;
const IMO_SAMPLE_THRESHOLD: u32 = 128 * 1024;

/// 按 croc 的算法名算一段数据的哈希，返回上线路的字节形态。
///
/// 各算法的上线路长度不同：xxhash 8 字节、md5 16 字节、imohash 16 字节、highway 32 字节。
/// 接收方按自己对同一算法的重算结果比对，所以**长度与字节序都必须与 Go 一致**。
///
/// **不认识的算法名返回 `None` 而不是退化成 xxhash**：那会让接收方拿到一个算法对不上
/// 的哈希，于是"内容相同"被误判成"需要重传"。
#[must_use]
pub fn hash_for_algorithm(algorithm: &str, data: &[u8]) -> Option<Vec<u8>> {
    match algorithm {
        "" | "xxhash" => Some(xxh64_bytes(data)),
        "md5" => Some(md5::Md5::digest(data).to_vec()),
        "imohash" => Some(imohash_bytes(data)),
        "highway" => Some(highway_bytes(HIGHWAY_KEY, data)),
        _ => None,
    }
}

/// 按算法名算一个文件的哈希。
///
/// 与 [`hash_for_algorithm`] 的区别：**imohash 走采样**，只读文件的头 / 中 / 尾各
/// 2 MiB，不把整个文件读进内存。imohash 存在的意义就是这个——用 `fs::read` 读全文件
/// 再喂给它，等于把它唯一的优点抹掉，还多占一份内存。
///
/// # Errors
///
/// 算法名不受支持、或文件读不出来时返回错误。
pub fn hash_file(algorithm: &str, path: &Path) -> Result<Vec<u8>, EngineError> {
    if algorithm == "imohash" {
        let hasher = imohash::Hasher::with_sample_size_and_threshold(IMO_SAMPLE_SIZE, IMO_SAMPLE_THRESHOLD);
        let path_text = path.to_str().ok_or_else(|| EngineError::file(path, "路径不是合法 UTF-8"))?;
        let digest = hasher.sum_file(path_text).map_err(|error| EngineError::file(path, error))?;
        return Ok(digest.to_le_bytes().to_vec());
    }
    // 其余算法要吃完整内容，只能整个读进来。
    let bytes = fs::read(path).map_err(|error| EngineError::file(path, error))?;
    hash_for_algorithm(algorithm, &bytes).ok_or_else(|| EngineError::UnsupportedHash(algorithm.to_owned()))
}

/// imohash：murmur3-128 采样哈希，再把文件长度以 uvarint 覆盖到摘要开头。
///
/// 返回的是 16 字节，与 Go 的 `imohash.SumFile` 逐字节一致。
#[must_use]
pub fn imohash_bytes(data: &[u8]) -> Vec<u8> {
    let hasher = imohash::Hasher::with_sample_size_and_threshold(IMO_SAMPLE_SIZE, IMO_SAMPLE_THRESHOLD);
    // `sum` 只在读取切片时失败（`Cursor` 上的读不会失败），这里就地降级成 0 摘要。
    let digest = hasher.sum(data).unwrap_or_default();
    digest.to_le_bytes().to_vec()
}

/// HighwayHash-256。密钥是 Go 硬编码的那 32 字节。
///
/// 输出 32 字节，4 个 u64 按**小端**排列——Go 的 `highwayhash.Sum` 就是这么写的
/// （`highwayhash_generic.go:191`）。
#[must_use]
pub fn highway_bytes(key: [u8; 32], data: &[u8]) -> Vec<u8> {
    let mut words = [0u64; 4];
    for (index, word) in words.iter_mut().enumerate() {
        let chunk: [u8; 8] =
            key.get(index * 8..index * 8 + 8).and_then(|slice| slice.try_into().ok()).unwrap_or_default();
        *word = u64::from_le_bytes(chunk);
    }
    // Go 的 `h.Sum(nil)` 就是 256 位那一路。`append` / `finalize256` 是 crate 的私有
    // inherent 方法，公开入口只有 `HighwayHash` trait 上的 `hash256`。
    highway::HighwayHasher::new(highway::Key(words)).hash256(data).iter().flat_map(|word| word.to_le_bytes()).collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    /// 官方参考实现的已知向量（种子 0）。
    ///
    /// 前三条是 XXH64 文档里给出的标准向量；后两条用参考实现复算过——**别凭印象填**，
    /// 手写哈希的测试里最容易出的错就是"期望值是自己编的"。
    #[test]
    fn known_vectors_match_the_reference() {
        assert_eq!(xxh64(b""), 0xEF46_DB37_51D8_E999);
        assert_eq!(xxh64(b"a"), 0xD24E_C4F1_A98C_6E5B);
        assert_eq!(xxh64(b"abc"), 0x44BC_2CF5_AD77_0999);
        assert_eq!(xxh64(b"message digest"), 0x066E_D728_FCEE_B3BE);
        assert_eq!(xxh64(b"abcdefghijklmnopqrstuvwxyz"), 0xCFE1_F278_FA89_835C);
    }

    #[test]
    fn the_stripe_boundary_is_handled() {
        // 31 / 32 / 33 字节正好卡在累加器开关的边界上。
        for size in [0usize, 1, 31, 32, 33, 63, 64, 65, 1000] {
            let data: Vec<u8> = (0..size).map(|index| u8::try_from(index % 251).unwrap_or(0)).collect();
            // 只是要求"不 panic 且结果稳定"，具体值由官方向量保证。
            assert_eq!(xxh64(&data), xxh64(&data), "size={size}");
        }
    }

    #[test]
    fn seeds_change_the_hash() {
        assert_ne!(xxh64_seeded(b"abc", 0), xxh64_seeded(b"abc", 1));
    }

    #[test]
    fn the_wire_form_is_big_endian() {
        // Go 的 `xxhash.Sum(nil)` 是大端。
        assert_eq!(xxh64_bytes(b""), 0xEF46_DB37_51D8_E999u64.to_be_bytes().to_vec());
        assert_eq!(xxh64_bytes(b"abc").len(), 8);
    }

    #[test]
    fn unsupported_algorithms_are_not_silently_downgraded() {
        assert!(hash_for_algorithm("", b"x").is_some());
        assert!(hash_for_algorithm("xxhash", b"x").is_some());
        // `imohash-v2` 是 Go 的渐进式哈希协商项，本实现没谈那条能力，所以不接受。
        assert!(hash_for_algorithm("imohash-v2", b"x").is_none());
        assert!(hash_for_algorithm("sha256", b"x").is_none());
        assert!(hash_for_algorithm("XXHASH", b"x").is_none(), "算法名区分大小写，与 Go 一致");
    }

    /// 测试输入：`第 i 字节 = (i*31+7) % 251`。与生成 Go 向量时用的公式一致。
    fn pattern(size: usize) -> Vec<u8> {
        (0..size).map(|index| u8::try_from((index * 31 + 7) % 251).unwrap_or(0)).collect()
    }

    /// Go `utils.HashFile` 的**实测**向量，四种算法逐个对齐。
    ///
    /// 取法：用 Go 调 `utils.HashFile` 对同一批输入算，把 hex 抄回来。**别凭印象改
    /// 这些值**——改了就等于把"与 Go 逐字节一致"这条断言删掉了，而哈希对不上的症状
    /// 只是"白传一遍"，不会报错，没人会注意到。
    ///
    /// `mid` 与 `large` 分别卡在 imohash 的两条边界上：`mid` 超过 128 KiB 的
    /// `sample_threshold` 但仍走全量（`size < 4*sample_size`），`large` 超过
    /// `4 * 2 MiB` 才真正走采样——**采样参数写错只有 `large` 能测出来**。
    const GO_VECTORS: [(&[u8], &str, &str, &str, &str); 3] = [
        (
            b"",
            "ef46db3751d8e999",
            "d41d8cd98f00b204e9800998ecf8427e",
            "00000000000000000000000000000000",
            "53ca6df1f553ea8033a064adce543d811a2952ea552d26b1420ec208e5f90103",
        ),
        (
            b"abc",
            "44bc2cf5ad770999",
            "900150983cd24fb0d6963f7d28e17f72",
            "03963f3f3fad78673ba2744126ca2d52",
            "532fc96223f9e74dc7a1ae53a0c64753bfbbf1bff64d2052d1479b8de7b1ff2b",
        ),
        (
            b"hello",
            "26c7827d889f6da3",
            "5d41402abc4b2a76b9719d911017c592",
            "05d8a7b341bd9b025b1e906a48ae1d19",
            "0a7a4787e2c947be7b2bca198a3dc82562b6037301b15a666c99c7e44b802658",
        ),
    ];

    /// 大输入单独一组：它们由 `pattern()` 生成，没法写进常量表。
    ///
    /// `1_000` 覆盖"超过 md5 一个填充块"，`300_000` 覆盖 imohash 的全量上界，
    /// `9_500_000` 覆盖 imohash 的采样路径。
    const BIG_VECTORS: [(usize, &str, &str, &str, &str); 3] = [
        (
            1_000,
            "9e3300c1cde3c58d",
            "e722bbb0c3185d08f1a31567e66a825b",
            "e80780d8d8d8f3a29e34715bf9c86070",
            "5ccd2c1ff7dc5e8ad6dae492da01eaf5b8843e586dae67485bf488d5f74989ae",
        ),
        (
            300_000,
            "041055457753fb47",
            "80aecaf206e64af6fc80802377378db8",
            "e0a712fc0c773896f4920350702ce83a",
            "2ad510ffb0c4a7719504cc159cacb22d60996b4277385e5860e0fcb08220f828",
        ),
        (
            9_500_000,
            "fcd14fcf755b5403",
            "f8e62e66ff42f17b410db2f0cc03fe9e",
            "e0eac304b806c2e190d757cf3e6bdee8",
            "4d7097b9271314813b7e3a51a005fd619ad6abd39a6c33761db0dc30873a8d3c",
        ),
    ];

    fn assert_matches_go(label: &str, data: &[u8], xxhash: &str, md5: &str, imohash: &str, highway: &str) {
        let expected = [("xxhash", xxhash), ("md5", md5), ("imohash", imohash), ("highway", highway)];
        for (algorithm, want) in expected {
            let got = hash_for_algorithm(algorithm, data).unwrap_or_else(|| panic!("{algorithm} 应受支持"));
            let hex: String = got.iter().map(|byte| format!("{byte:02x}")).collect();
            assert_eq!(hex, want, "{label} 的 {algorithm} 与 Go 不一致");
        }
    }

    #[test]
    fn all_four_algorithms_match_the_go_vectors() {
        for (data, xxhash, md5, imohash, highway) in GO_VECTORS {
            let label = format!("{:?}", std::str::from_utf8(data).unwrap_or("<binary>"));
            assert_matches_go(&label, data, xxhash, md5, imohash, highway);
        }
        for (size, xxhash, md5, imohash, highway) in BIG_VECTORS {
            let data = pattern(size);
            assert_matches_go(&format!("pattern({size})"), &data, xxhash, md5, imohash, highway);
        }
    }

    #[test]
    fn the_digest_lengths_are_the_go_ones() {
        // 上线路长度错了，对端只会当成"哈希不同"，不会报错。
        assert_eq!(hash_for_algorithm("xxhash", b"x").map(|v| v.len()), Some(8));
        assert_eq!(hash_for_algorithm("md5", b"x").map(|v| v.len()), Some(16));
        assert_eq!(hash_for_algorithm("imohash", b"x").map(|v| v.len()), Some(16));
        assert_eq!(hash_for_algorithm("highway", b"x").map(|v| v.len()), Some(32));
    }

    #[test]
    fn imohash_file_sampling_agrees_with_the_in_memory_path() {
        // `hash_file` 走的是"只读头/中/尾"的采样路径，`hash_for_algorithm` 走内存切片。
        // 两条路径必须给出同一个摘要，否则 `--hash imohash` 的结果取决于怎么调用。
        let data = pattern(9_500_000);
        let directory = std::env::temp_dir().join("croc-rs-hash-file-test");
        fs::create_dir_all(&directory).expect("应能建临时目录");
        let path = directory.join("sampled.bin");
        fs::write(&path, &data).expect("应能写临时文件");

        let from_file = hash_file("imohash", &path).expect("采样哈希应成功");
        assert_eq!(from_file, hash_for_algorithm("imohash", &data).expect("应受支持"));
        assert_eq!(from_file.len(), 16);

        let _ = fs::remove_file(&path);
        let _ = fs::remove_dir(&directory);
    }
}
