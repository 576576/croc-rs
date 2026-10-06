//! 能力协商：PAKE 阶段双方交换的 `f` 列表。
//!
//! 对应 Go 里散落在 `croc.go`、`file_manifest.go`、`tailcat_negotiation.go` 的那一堆
//! `const ... = "..."` 字符串，以及 `supportsFeature`。Go 用裸字符串，写错一个字母只会
//! 在运行时静默退化成"对端不支持"——最难查的一类 bug。这里换成枚举，拼错就是编译错误。
//!
//! 但对端发来的列表仍然是**任意字符串**：新版对端可能宣告本端还不认识的能力，而我们
//! 必须原样接受、原样忽略。所以协商函数收 `&[String]`，不收 `&[Feature]`。
//!
//! # 宣告顺序是协议的一部分
//!
//! [`declare`] 的输出顺序与 Go 的 `pakeFeatures()` 逐字一致。顺序本身不影响语义
//! （对端用 `slices.Contains` 查找），但保持一致能让 golden 向量做字节级比较。

use std::fmt;

/// 一个已知的能力名。
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, PartialOrd, Ord)]
pub enum Feature {
    /// 逐文件压缩开关（`SenderInfo.Features` 里用它回传）。
    PerFileCompression,
    /// 发送方信息里内联对端元数据（machine id、外网 IP 等）。
    InlinePeerMetadata,
    /// 边收边算哈希，收完即可校验。
    ProgressiveFileHash,
    /// 文件清单分块传输，避免超大目录撑爆单条消息。
    ChunkedFileInfo,
    /// 分阶段选择数据通道（先建 relay，再按需切到 tailcat）。
    StagedTransport,
    /// 对端可以不等显式的 tailcat 就绪信号就开传。
    ImplicitTailcatReady,
    /// 支持 tailcat 数据通道（实验性）。
    ExperimentalTailcat,
    /// 要求对端也必须支持 tailcat（实验性）。
    ExperimentalTailcatRequired,
    /// SSH 终端共享的协商标记。
    SshRendezvous,
}

impl Feature {
    /// 全部已知能力。**顺序与 Go 的声明顺序无关**，只用于 [`Feature::parse`] 查找。
    pub const ALL: [Self; 9] = [
        Self::PerFileCompression,
        Self::InlinePeerMetadata,
        Self::ProgressiveFileHash,
        Self::ChunkedFileInfo,
        Self::StagedTransport,
        Self::ImplicitTailcatReady,
        Self::ExperimentalTailcat,
        Self::ExperimentalTailcatRequired,
        Self::SshRendezvous,
    ];

    /// 线上的能力名，必须与 Go 的常量逐字相同。
    #[must_use]
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::PerFileCompression => "per-file-compression-v1",
            Self::InlinePeerMetadata => "inline-peer-metadata-v1",
            Self::ProgressiveFileHash => "progressive-file-hash-v1",
            Self::ChunkedFileInfo => "chunked-fileinfo-v1",
            Self::StagedTransport => "staged-transport-v1",
            Self::ImplicitTailcatReady => "implicit-tailcat-ready-v1",
            Self::ExperimentalTailcat => "experimental-tailcat-v1",
            Self::ExperimentalTailcatRequired => "experimental-tailcat-required-v1",
            Self::SshRendezvous => "ssh-rendezvous-v1",
        }
    }

    /// 从线上名字解析；不认识的返回 `None`（不是错误，是正常的前向兼容）。
    #[must_use]
    pub fn parse(name: &str) -> Option<Self> {
        Self::ALL.into_iter().find(|feature| feature.as_str() == name)
    }
}

impl fmt::Display for Feature {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter.write_str(self.as_str())
    }
}

/// 对端宣告的能力列表里是否有 `wanted`。
///
/// 与 Go 的 `supportsFeature` 等价，但用枚举参数挡住拼写错误。
#[must_use]
pub fn supports(features: &[String], wanted: Feature) -> bool {
    let name = wanted.as_str();
    features.iter().any(|feature| feature == name)
}

/// 把对端列表里**本端认识**的部分挑出来。
///
/// 用于日志和诊断：不认识的条目会被安静丢弃，而不是让上层去处理 `&[String]`。
pub fn known(features: &[String]) -> impl Iterator<Item = Feature> + '_ {
    features.iter().filter_map(|feature| Feature::parse(feature))
}

/// 本端 tailcat 支持情况，对应 Go 的 `localTailcatSupported()` + `Options.Transport`。
///
/// 本 crate 拿不到构建配置和命令行参数，所以让调用方把结论算好再传进来。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum TailcatDeclaration {
    /// 本端不支持 tailcat：`--transport relay`、`--only-local`，或构建里没编进 tailcat。
    Unsupported,
    /// 本端支持，但不强制对端也用。
    Optional,
    /// 发送端 + `--transport derp`：要求对端也必须支持。
    Required,
}

/// 本端在 PAKE 阶段宣告的能力列表。
///
/// 对应 Go 的 `pakeFeatures()`，顺序逐字一致：五个基础能力，然后按 tailcat 支持情况
/// 追加一到两个实验性能力。
#[must_use]
pub fn declare(tailcat: TailcatDeclaration) -> Vec<String> {
    let mut features = vec![
        Feature::InlinePeerMetadata.as_str().to_owned(),
        Feature::ProgressiveFileHash.as_str().to_owned(),
        Feature::ChunkedFileInfo.as_str().to_owned(),
        Feature::StagedTransport.as_str().to_owned(),
        Feature::ImplicitTailcatReady.as_str().to_owned(),
    ];
    match tailcat {
        TailcatDeclaration::Unsupported => {}
        TailcatDeclaration::Optional => features.push(Feature::ExperimentalTailcat.as_str().to_owned()),
        TailcatDeclaration::Required => {
            features.push(Feature::ExperimentalTailcat.as_str().to_owned());
            features.push(Feature::ExperimentalTailcatRequired.as_str().to_owned());
        }
    }
    features
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn wire_names_match_the_go_constants() {
        // 这些字符串是硬契约：改一个字就会让互通静默降级。
        let expected = [
            ("per-file-compression-v1", Feature::PerFileCompression),
            ("inline-peer-metadata-v1", Feature::InlinePeerMetadata),
            ("progressive-file-hash-v1", Feature::ProgressiveFileHash),
            ("chunked-fileinfo-v1", Feature::ChunkedFileInfo),
            ("staged-transport-v1", Feature::StagedTransport),
            ("implicit-tailcat-ready-v1", Feature::ImplicitTailcatReady),
            ("experimental-tailcat-v1", Feature::ExperimentalTailcat),
            ("experimental-tailcat-required-v1", Feature::ExperimentalTailcatRequired),
            ("ssh-rendezvous-v1", Feature::SshRendezvous),
        ];
        for (name, feature) in expected {
            assert_eq!(feature.as_str(), name);
            assert_eq!(Feature::parse(name), Some(feature));
        }
        assert_eq!(Feature::ALL.len(), expected.len());
    }

    #[test]
    fn parsing_round_trips_every_known_feature() {
        for feature in Feature::ALL {
            assert_eq!(Feature::parse(feature.as_str()), Some(feature), "{feature} 应能往返");
            assert_eq!(feature.to_string(), feature.as_str());
        }
    }

    #[test]
    fn parsing_rejects_unknown_names() {
        assert_eq!(Feature::parse("chunked-file-info-v1"), None);
        assert_eq!(Feature::parse(""), None);
        assert_eq!(Feature::parse("INLINE-PEER-METADATA-V1"), None);
    }

    #[test]
    fn declaration_order_matches_go() {
        assert_eq!(
            declare(TailcatDeclaration::Unsupported),
            vec![
                "inline-peer-metadata-v1",
                "progressive-file-hash-v1",
                "chunked-fileinfo-v1",
                "staged-transport-v1",
                "implicit-tailcat-ready-v1",
            ]
        );
        assert_eq!(
            declare(TailcatDeclaration::Optional),
            vec![
                "inline-peer-metadata-v1",
                "progressive-file-hash-v1",
                "chunked-fileinfo-v1",
                "staged-transport-v1",
                "implicit-tailcat-ready-v1",
                "experimental-tailcat-v1",
            ]
        );
        assert_eq!(
            declare(TailcatDeclaration::Required),
            vec![
                "inline-peer-metadata-v1",
                "progressive-file-hash-v1",
                "chunked-fileinfo-v1",
                "staged-transport-v1",
                "implicit-tailcat-ready-v1",
                "experimental-tailcat-v1",
                "experimental-tailcat-required-v1",
            ]
        );
    }

    #[test]
    fn declaring_never_mentions_relay_only_features() {
        // 五个基础能力里不应出现 tailcat 或 ssh 相关项——Go 的列表里就没有。
        for declaration in [TailcatDeclaration::Unsupported, TailcatDeclaration::Optional] {
            let features = declare(declaration);
            assert!(!supports(&features, Feature::SshRendezvous));
            assert!(!supports(&features, Feature::PerFileCompression));
        }
    }

    #[test]
    fn supports_is_exact_match_only() {
        let features = vec!["inline-peer-metadata-v1".to_owned()];
        assert!(supports(&features, Feature::InlinePeerMetadata));
        assert!(!supports(&features, Feature::ProgressiveFileHash));
        assert!(!supports(&features, Feature::ExperimentalTailcat));

        // 前缀不算命中。
        let partial = vec!["experimental-tailcat".to_owned()];
        assert!(!supports(&partial, Feature::ExperimentalTailcat));
        assert!(!supports(&partial, Feature::ExperimentalTailcatRequired));
    }

    #[test]
    fn unknown_entries_are_dropped_but_do_not_break_negotiation() {
        let features =
            vec!["from-the-future-v9".to_owned(), "staged-transport-v1".to_owned(), "also-unknown".to_owned()];
        assert!(supports(&features, Feature::StagedTransport));
        assert_eq!(known(&features).collect::<Vec<_>>(), vec![Feature::StagedTransport]);
    }
}
