//! 公共中继池：地址表，以及"哪一台能用"的探测。
//!
//! Go 版在 `src/publicrelay` 里定义了一个有序的池，并把口令码按
//! `sha256(码) mod 池长` 确定性地分到其中一台（`src/codephrase` 的 `RelayIndex`）。
//! 于是码本身就带着"该连哪台"这层信息——对端只要拿到码就能算出同一个索引，不需要
//! 谁来通知它。
//!
//! 不做这一层的后果不是报错，而是静默挂住：发送方连默认中继，接收方按码算出另一
//! 个索引于是连上另一台。两台机器各在自己那个（房间名完全正确、也确实空的）房间里等
//! 一个永远不会来的人，表现出来就是接收端一直停在 "Authenticating code"。

use std::time::{Duration, Instant};

use croc_net::Framed;
use croc_protocol::relay::{PING, PONG};

use crate::dns::Mode;
use crate::error::EngineError;
use crate::proxy::Proxy;
use crate::relay;

/// 池里的中继，顺序即索引。
///
/// 改顺序或增减条目会让所有在途的码指向别的中继，所以对端版本不一致时会互相等不到。
pub const RELAYS: &[&str] = &["1.getcroc.com:9009", "2.getcroc.com:9009", "3.getcroc.com:9009", "4.getcroc.com:9009"];

/// 单次探测的超时。Go 的 `publicrelay.ProbeTimeout`。
///
/// 一秒是因为这只是"谁先回"的竞速，不是要测准延迟；慢的那台并不是不能用。
pub const PROBE_TIMEOUT: Duration = Duration::from_secs(1);

/// 对一台中继做一次完整的 ping/pong，返回往返耗时。
///
/// 走的是中继的探活分支（`croc_protocol::relay` 里 `ping` → `pong`）：不加密、不进
/// 房间，中继回完就关连接。
///
/// # Errors
///
/// 地址写错、连不上、超时、或者回的不是 `pong`。
pub async fn probe(address: &str, proxy: &Proxy, dns: Mode) -> Result<Duration, EngineError> {
    let (host, port) = relay::split_host_port(address)?;
    let started = Instant::now();
    let stream = relay::dial(&host, port, proxy, dns).await?;
    let mut framed = Framed::new(stream);
    framed.write_frame(PING).await?;
    let elapsed = started.elapsed();
    match framed.read_frame().await? {
        Some(frame) if frame == PONG => Ok(elapsed),
        // 回的不是 `pong`：对方多半不是 croc 中继，或者是一个改过探活语义的版本。
        Some(_) => Err(EngineError::Relay(format!("{address} answered the ping with something else"))),
        None => Err(EngineError::Relay(format!("{address} closed the connection during the ping"))),
    }
}

/// 同时探测池里每一台，返回第一个完成 ping/pong 的那台的索引。
///
/// Go 的 `publicrelay.SelectFirst` 是同一套：全部并发起跑，谁先成功用谁，其余立刻
/// 取消。失败的（连不上 / 超时）跳过——只有全部失败才算错。
///
/// # Errors
///
/// 池是空的，或者每一台都探不到（返回最后一条错误）。
pub async fn select_first(pool: &[String], proxy: &Proxy, dns: Mode) -> Result<usize, EngineError> {
    let mut pending = tokio::task::JoinSet::new();
    for (index, address) in pool.iter().enumerate() {
        let address = address.clone();
        let proxy = proxy.clone();
        pending.spawn(async move {
            let result = tokio::time::timeout(PROBE_TIMEOUT, probe(&address, &proxy, dns)).await;
            (index, result)
        });
    }

    let mut last = None;
    while let Some(joined) = pending.join_next().await {
        match joined {
            Ok((index, Ok(Ok(_)))) => {
                // 已经有人赢了：其余探测没必要再占着连接。
                pending.shutdown().await;
                return Ok(index);
            }
            Ok((_, Ok(Err(error)))) => last = Some(error),
            Ok((_, Err(_elapsed))) => {
                last = Some(EngineError::Relay(format!("probe timed out after {PROBE_TIMEOUT:?}")));
            }
            Err(error) => last = Some(EngineError::Config(format!("the probe task {error}"))),
        }
    }
    Err(last.unwrap_or_else(|| EngineError::Config("the public relay pool is empty".to_owned())))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_pool_matches_go() {
        // 顺序即索引，改一条就会让两边的码指向不同的中继。
        assert_eq!(RELAYS, ["1.getcroc.com:9009", "2.getcroc.com:9009", "3.getcroc.com:9009", "4.getcroc.com:9009"]);
    }

    #[tokio::test]
    async fn an_empty_pool_is_rejected() {
        assert!(select_first(&[], &Proxy::Direct, Mode::System).await.is_err());
    }

    #[tokio::test]
    async fn every_relay_that_replies_wins() {
        // 起一台本地中继当池里唯一的一台，探测应当成功并选中索引 0。
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.expect("应能监听");
        let address = listener.local_addr().expect("应能取到地址");
        let server = tokio::spawn(async move {
            let (stream, _) = listener.accept().await.expect("应能接受");
            let mut framed = Framed::new(stream);
            let frame = framed.read_frame().await.expect("应能读").expect("不该是 EOF");
            assert_eq!(frame, PING);
            framed.write_frame(PONG).await.expect("应能写");
        });
        let pool = vec![address.to_string()];
        assert_eq!(select_first(&pool, &Proxy::Direct, Mode::System).await.expect("应能选中"), 0);
        server.await.expect("服务端任务应当正常结束");
    }
}
