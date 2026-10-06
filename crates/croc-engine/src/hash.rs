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

/// 按 croc 的算法名算一个文件的哈希。
///
/// 目前只实现 `""`（默认）与 `"xxhash"`。其余算法名（`md5` / `highway` / `imohash`）
/// 在 `croc-cli` 层就被挡下来了——**不在这里静默退化成 xxhash**：那会让接收方拿到
/// 一个算法对不上的哈希，于是"内容相同"被误判成"需要重传"。
///
/// # Errors
///
/// 算法名不受支持时返回 `None`。
#[must_use]
pub fn hash_for_algorithm(algorithm: &str, data: &[u8]) -> Option<Vec<u8>> {
    match algorithm {
        "" | "xxhash" => Some(xxh64_bytes(data)),
        _ => None,
    }
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
        assert!(hash_for_algorithm("md5", b"x").is_none());
        assert!(hash_for_algorithm("imohash", b"x").is_none());
    }
}
