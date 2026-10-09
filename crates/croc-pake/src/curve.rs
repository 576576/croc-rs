//! 曲线选择与固定点 `U`/`V`。
//!
//! `schollz/pake` v3 支持五条曲线，每条配一对写死的 `U`/`V`（SPAKE2 里用来把口令
//! 盲化的固定点）。这些常量在 `initCurve()` 里硬编码，且只在初始化时校验过一次
//! 是否在曲线上——本模块用单元测试把这条校验固化下来。
//!
//! `ed25519` 是个特例：它的 `U`/`V` 是「用种子 `croc1`/`croc2` 生成的固定点」，
//! 点整体塞在 `x` 坐标里，`y` 恒为 0。

use croc_siec::params::{nist_p256, nist_p384, nist_p521, siec255};
use croc_siec::{AffinePoint, WeierstrassCurve, edwards, parse_int};

use crate::error::PakeError;

/// Go 版 `pake.AvailableCurves()` 支持的五条曲线。
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum CurveKind {
    /// NIST P-521。
    P521,
    /// NIST P-256，croc CLI 的默认值。
    P256,
    /// NIST P-384。
    P384,
    /// SIEC255，`tscholl2/siec` 的短 Weierstrass 曲线。
    Siec,
    /// Edwards25519。
    Ed25519,
}

impl CurveKind {
    /// 全部曲线，顺序与 Go 版 `AvailableCurves()` 一致（`--curve` 的帮助文本按这个
    /// 顺序拼出来）。
    pub const ALL: [Self; 5] = [Self::P521, Self::P256, Self::P384, Self::Siec, Self::Ed25519];

    /// 按 Go 版曲线名解析。
    pub fn from_name(name: &str) -> Result<Self, PakeError> {
        match name {
            "p521" => Ok(Self::P521),
            "p256" => Ok(Self::P256),
            "p384" => Ok(Self::P384),
            "siec" => Ok(Self::Siec),
            "ed25519" => Ok(Self::Ed25519),
            other => Err(PakeError::UnknownCurve(other.to_owned())),
        }
    }

    /// 线路上的曲线名（也是 `pakekey` 转录哈希里用的那个字符串）。
    pub fn name(self) -> &'static str {
        match self {
            Self::P521 => "p521",
            Self::P256 => "p256",
            Self::P384 => "p384",
            Self::Siec => "siec",
            Self::Ed25519 => "ed25519",
        }
    }

    /// 本曲线是否按 Go `nistCurve.normalizeScalar` 预处理标量。
    ///
    /// `siec` 与 `ed25519` 都是 `false`：前者的 `ScalarMult` 直接吃原始字节，后者走
    /// 自己的 32 字节钳位路径。
    pub fn normalizes_scalars(self) -> bool {
        matches!(self, Self::P256 | Self::P384 | Self::P521)
    }
}

impl core::fmt::Display for CurveKind {
    fn fmt(&self, formatter: &mut core::fmt::Formatter<'_>) -> core::fmt::Result {
        formatter.write_str(self.name())
    }
}

/// 底层算术引擎。
#[derive(Debug)]
enum Engine {
    Weierstrass(&'static WeierstrassCurve),
    Edwards25519,
}

/// 一条可用的 PAKE 曲线：引擎 + 固定点 `U`/`V`。
#[derive(Debug)]
pub struct Curve {
    kind: CurveKind,
    engine: Engine,
    u: AffinePoint,
    v: AffinePoint,
}

impl Curve {
    /// 按曲线类型构造。
    pub fn new(kind: CurveKind) -> Self {
        let (engine, fixed) = match kind {
            CurveKind::P521 => (Engine::Weierstrass(nist_p521()), FIXED_P521),
            CurveKind::P256 => (Engine::Weierstrass(nist_p256()), FIXED_P256),
            CurveKind::P384 => (Engine::Weierstrass(nist_p384()), FIXED_P384),
            CurveKind::Siec => (Engine::Weierstrass(siec255()), FIXED_SIEC),
            CurveKind::Ed25519 => (Engine::Edwards25519, FIXED_ED25519),
        };
        Self { kind, engine, u: fixed_point(fixed.u_x, fixed.u_y), v: fixed_point(fixed.v_x, fixed.v_y) }
    }

    /// 曲线类型。
    pub fn kind(&self) -> CurveKind {
        self.kind
    }

    /// 线路上的曲线名。
    pub fn name(&self) -> &'static str {
        self.kind.name()
    }

    /// 固定点 `U`。
    pub fn u(&self) -> &AffinePoint {
        &self.u
    }

    /// 固定点 `V`。
    pub fn v(&self) -> &AffinePoint {
        &self.v
    }

    /// 点是否在曲线上。
    pub fn is_on_curve(&self, point: &AffinePoint) -> bool {
        match self.engine {
            Engine::Weierstrass(curve) => curve.is_on_curve(point),
            Engine::Edwards25519 => edwards::is_on_curve(point),
        }
    }

    /// 点加。
    pub fn add(&self, first: &AffinePoint, second: &AffinePoint) -> AffinePoint {
        match self.engine {
            Engine::Weierstrass(curve) => curve.add(first, second),
            Engine::Edwards25519 => edwards::add(first, second),
        }
    }

    /// 点减 `first - second`。
    ///
    /// Weierstrass 侧照抄 Go 的写法：先把 `second` 的 `y` 取反再相加（
    /// `Add(x, y, x₂, -y₂)`），而不是调用什么 `Sub`。Ed25519 侧用 Go 为它专门加的
    /// `Subtract`——两条路径的实现不同，结果必须一致，这是 pake v3 的实际代码。
    pub fn subtract(&self, first: &AffinePoint, second: &AffinePoint) -> AffinePoint {
        match self.engine {
            Engine::Weierstrass(curve) => curve.add(first, &curve.negate(second)),
            Engine::Edwards25519 => edwards::subtract(first, second),
        }
    }

    /// 标量乘，按曲线各自的标量预处理规则。
    pub fn scalar_mult(&self, point: &AffinePoint, scalar: &[u8]) -> AffinePoint {
        match self.engine {
            Engine::Weierstrass(curve) if curve.normalize_scalars() => {
                curve.scalar_mult(point, &curve.normalize_scalar(scalar))
            }
            Engine::Weierstrass(curve) => curve.scalar_mult(point, scalar),
            Engine::Edwards25519 => edwards::scalar_mult(point, scalar),
        }
    }

    /// 基点标量乘 `k·G`。
    pub fn scalar_mult_base(&self, scalar: &[u8]) -> AffinePoint {
        match self.engine {
            Engine::Weierstrass(curve) if curve.normalize_scalars() => {
                curve.scalar_mult_base(&curve.normalize_scalar(scalar))
            }
            Engine::Weierstrass(curve) => curve.scalar_mult_base(scalar),
            Engine::Edwards25519 => edwards::scalar_mult_base(scalar),
        }
    }
}

/// 一对固定点的十进制字面量。
struct FixedPoints {
    u_x: &'static str,
    u_y: &'static str,
    v_x: &'static str,
    v_y: &'static str,
}

fn fixed_point(x: &str, y: &str) -> AffinePoint {
    AffinePoint::new(parse_int(x).unwrap_or_default(), parse_int(y).unwrap_or_default())
}

const FIXED_P521: FixedPoints = FixedPoints {
    u_x: "793136080485469241208656611513609866400481671852",
    u_y: "4032821203812196944795502391345776760852202059010382256134592838722123385325802540879231526503456158741518531456199762365161310489884151533417829496019094620",
    v_x: "1086685267857089638167386722555472967068468061489",
    v_y: "5010916268086655347194655708160715195931018676225831839835602465999566066450501167246678404591906342753230577187831311039273858772817427392089150297708931207",
};

const FIXED_P256: FixedPoints = FixedPoints {
    u_x: "793136080485469241208656611513609866400481671852",
    u_y: "59748757929350367369315811184980635230185250460108398961713395032485227207304",
    v_x: "1086685267857089638167386722555472967068468061489",
    v_y: "9157340230202296554417312816309453883742349874205386245733062928888341584123",
};

const FIXED_P384: FixedPoints = FixedPoints {
    u_x: "793136080485469241208656611513609866400481671852",
    u_y: "7854890799382392388170852325516804266858248936799429260403044177981810983054351714387874260245230531084533936948596",
    v_x: "1086685267857089638167386722555472967068468061489",
    v_y: "21898206562669911998235297167979083576432197282633635629145270958059347586763418294901448537278960988843108277491616",
};

/// 注意 `siec` 的 `U_x` 结尾是 `...853`，其余四条曲线是 `...852`。
/// 这是参考实现里的真实差异，不是笔误。
const FIXED_SIEC: FixedPoints = FixedPoints {
    u_x: "793136080485469241208656611513609866400481671853",
    u_y: "18458907634222644275952014841865282643645472623913459400556233196838128612339",
    v_x: "1086685267857089638167386722555472967068468061489",
    v_y: "19593504966619549205903364028255899745298716108914514072669075231742699650911",
};

/// Ed25519 的固定点是压缩点的大端整数，`y` 恒为 0。
const FIXED_ED25519: FixedPoints = FixedPoints {
    u_x: "41821174510521985817056358996007359290163947216650231187782646151092828043509",
    u_y: "0",
    v_x: "1456941786990260824647297143563623381366314063537015067473110401627488371271",
    v_y: "0",
};

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn every_curve_name_round_trips() {
        for kind in CurveKind::ALL {
            assert_eq!(CurveKind::from_name(kind.name()), Ok(kind));
        }
        assert_eq!(CurveKind::from_name("nope"), Err(PakeError::UnknownCurve("nope".to_owned())));
    }

    #[test]
    fn available_curves_match_go_order() {
        let names: Vec<&str> = CurveKind::ALL.iter().map(|kind| kind.name()).collect();
        assert_eq!(names, vec!["p521", "p256", "p384", "siec", "ed25519"]);
    }

    #[test]
    fn fixed_points_lie_on_their_curves() {
        // Go 的 `initCurve()` 在初始化时做同样的校验，失败会返回 `Ux/Uy not on curve`。
        // 常量抄错一位，这里就会红。
        for kind in CurveKind::ALL {
            let curve = Curve::new(kind);
            assert!(curve.is_on_curve(curve.u()), "{} 的 U 不在曲线上", kind.name());
            assert!(curve.is_on_curve(curve.v()), "{} 的 V 不在曲线上", kind.name());
        }
    }

    #[test]
    fn scalar_normalization_follows_go() {
        assert!(CurveKind::P256.normalizes_scalars());
        assert!(CurveKind::P384.normalizes_scalars());
        assert!(CurveKind::P521.normalizes_scalars());
        assert!(!CurveKind::Siec.normalizes_scalars());
        assert!(!CurveKind::Ed25519.normalizes_scalars());
    }

    #[test]
    fn scalar_mult_by_one_returns_the_base_point() {
        // SIEC255 的基点是公开常量 (5, 12)，可以直接比对。
        let siec = Curve::new(CurveKind::Siec);
        assert_eq!(
            siec.scalar_mult_base(&[1u8]),
            AffinePoint::new(parse_int("5").unwrap_or_default(), parse_int("12").unwrap_or_default())
        );
        for kind in [CurveKind::P256, CurveKind::P384, CurveKind::P521, CurveKind::Ed25519] {
            let curve = Curve::new(kind);
            // 标量 1 在 NIST 曲线上会走 normalize_scalar，结果仍是 1。
            let point = curve.scalar_mult_base(&[1u8]);
            assert!(curve.is_on_curve(&point), "{} 的 [1]G 应在曲线上", kind.name());
        }
    }

    #[test]
    fn subtracting_then_adding_restores_the_original_point() {
        for kind in CurveKind::ALL {
            let curve = Curve::new(kind);
            let left = curve.scalar_mult_base(&[9u8; 32]);
            let right = curve.scalar_mult_base(&[4u8; 32]);
            let difference = curve.subtract(&left, &right);
            assert_eq!(curve.add(&difference, &right), left, "{} 的加减不互逆", kind.name());
        }
    }
}
