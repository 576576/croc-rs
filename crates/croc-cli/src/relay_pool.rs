//! 公共中继池的命令行侧：该不该走池、选哪一台、码落在哪一台。
//!
//! 池的地址与探测在 `croc_engine::public_relay`；这里管的是跟命令行开关缠在一起的那几个决定。

use croc_core::codephrase;
use croc_engine::dns::Mode;
use croc_engine::proxy::Proxy;

use crate::cli::{DEFAULT_RELAY, DEFAULT_RELAY6};
use crate::config;

/// 池里的中继，顺序即索引。
const POOL: &[&str] = croc_engine::public_relay::RELAYS;

/// 这次走不走公共中继池。
///
/// Go 的 `usesPublicRelay`：两个中继参数都没在命令行/环境里给过、不是
/// `--local`、没给 `--ip`，而且合并（含 `--remember`）出来的地址还是默认值。
///
/// 少了这条判断会怎样：用户显式 `--relay` 指向自建中继时，码里的索引会把对端送到
/// `1..4.getcroc.com` 上去，两端各在一处，谁也等不到谁。
#[must_use]
pub fn applies(given_relay: bool, given_relay6: bool, local: bool, ip: &str, relay: &str, relay6: &str) -> bool {
    !given_relay
        && !given_relay6
        && !local
        && ip.trim().is_empty()
        && relay == DEFAULT_RELAY
        && relay6 == DEFAULT_RELAY6
}

/// 池的长度。
#[must_use]
pub const fn count() -> u32 {
    POOL.len() as u32
}

/// 这个码落在池里的第几台。两端各自算，结果必须一致。
///
/// # Errors
///
/// 池是空的（代码写错时才会发生），或者索引窄化失败。
pub fn index_of(code: &str) -> Result<usize, String> {
    let index = codephrase::relay_index(code, count()).map_err(|error| format!("croc-rs: {error}"))?;
    usize::try_from(index).map_err(|_| format!("croc-rs: 中继索引 {index} 超出范围"))
}

/// 池里第 `index` 台的地址。
///
/// # Errors
///
/// 索引越界。
pub fn address(index: usize) -> Result<&'static str, String> {
    POOL.get(index).copied().ok_or_else(|| format!("croc-rs: 公共中继池里没有第 {index} 台"))
}

/// 挑一台：先读上次记住的那台，没有就探测一遍并记下来。
///
/// # Errors
///
/// 池里每一台都探不到。
pub fn select(proxy: &Proxy, internal_dns: bool, quiet: bool) -> Result<usize, String> {
    if let Some(index) = config::load_best_public_relay(POOL) {
        return Ok(index);
    }
    let dns = if internal_dns { Mode::Internal } else { Mode::System };
    let index = probe(proxy, dns)?;
    let Some(address) = POOL.get(index).copied() else {
        return Err(format!("croc-rs: 公共中继池里没有第 {index} 台"));
    };
    config::save_best_public_relay(address, quiet);
    Ok(index)
}

/// 探测一遍池里每一台，返回最先应答的那台的索引。
///
/// `parse()` 在 tokio 运行时起来之前就跑完了，所以这里自己开一个临时的运行时。
/// 一次探测最多 [`croc_engine::public_relay::PROBE_TIMEOUT`]，开一个单线程运行时比把
/// 整个参数解析改成异步划算得多。
///
/// # Errors
///
/// 运行时起不来，或者每一台都探不到。
fn probe(proxy: &Proxy, dns: Mode) -> Result<usize, String> {
    let runtime = tokio::runtime::Builder::new_current_thread()
        .enable_all()
        .build()
        .map_err(|error| format!("croc-rs: 无法启动中继探测：{error}"))?;
    let pool: Vec<String> = POOL.iter().map(|relay| (*relay).to_owned()).collect();
    runtime
        .block_on(croc_engine::public_relay::select_first(&pool, proxy, dns))
        .map_err(|error| format!("croc-rs: 公共中继都探不到：{error}"))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_pool_has_four_relays() {
        assert_eq!(count(), 4);
    }

    #[test]
    fn every_index_round_trips_through_its_address() {
        for (index, relay) in POOL.iter().enumerate() {
            assert_eq!(address(index).expect("索引应当存在"), *relay);
        }
        assert!(address(POOL.len()).is_err());
    }

    #[test]
    fn a_code_maps_into_the_pool() {
        // 索引必须在池内，否则两端会算出不同的地址。
        for code in ["abc-def-ghi", "quirk-yolk-pouch", "yo-yo-acorn-acid"] {
            let index = index_of(code).expect("应能算出索引");
            assert!(index < POOL.len(), "{code} → {index}");
        }
    }

    #[test]
    fn the_same_code_always_maps_to_the_same_relay() {
        assert_eq!(index_of("abc-def-ghi").expect("应能算出索引"), index_of("abc-def-ghi").expect("应能算出索引"));
    }

    #[test]
    fn an_explicit_relay_disables_the_pool() {
        assert!(applies(false, false, false, "", DEFAULT_RELAY, DEFAULT_RELAY6));
        assert!(!applies(true, false, false, "", DEFAULT_RELAY, DEFAULT_RELAY6), "--relay 是显式给的");
        assert!(!applies(false, false, false, "", "10.0.0.1:9009", DEFAULT_RELAY6), "地址不是默认值");
        assert!(!applies(false, false, true, "", DEFAULT_RELAY, DEFAULT_RELAY6), "--local");
        assert!(!applies(false, false, false, "10.0.0.1:9009", DEFAULT_RELAY, DEFAULT_RELAY6), "--ip");
    }
}
