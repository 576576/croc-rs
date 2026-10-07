//! Go `encoding/json` 对 `pake.Pake` 公开字段的编码。
//!
//! # 为什么手写而不用 serde
//!
//! 参考实现的线路值是 `json.Marshal(p.Public())`，它有三个怪癖：
//!
//! 1. **`*big.Int` 序列化成裸十进制数字**（不加引号），`nil` 则是 `null`；
//! 2. 字段名带 Unicode 下标字符（`U` + U+1D64 / U+1D65，即 `Uᵤ`/`Uᵥ`）；
//! 3. 字段顺序由结构体**声明顺序**决定：`Role, Uᵤ, Uᵥ, Vᵤ, Vᵥ, Xᵤ, Xᵥ, Yᵤ, Yᵥ`。
//!
//! 用 `serde_json` 复现第 1 条需要开 `arbitrary_precision`——那是个会污染整个依赖图
//! 的全局 feature，对密码学核心 crate 来说不值当。所以这里手写：编码保证逐字节一致，
//! 解码是一个只认「我们需要的子集」的最小 JSON 读取器。
//!
//! 字段名一律用 [`FIELD_U_X`] 这样的常量 + `\u{1d64}` 转义给出。直接在源码里敲
//! `ᵤ` 和 `ᤤ` 长得几乎一样，抄错一个码点就是一次静默的互通失败。
//!
//! 这套怪癖集中在一个模块里，而不是散落成 `// COMPAT:` 注释——见
//! `docs/agent/plan/02-crypto.md` §6「丑的地方要被封装，而不是被注释」。

use croc_siec::{AffinePoint, parse_int};
use num_bigint::BigInt;
use num_traits::ToPrimitive;

use crate::error::PakeError;

/// `"Role"`。
pub const FIELD_ROLE: &str = "Role";
/// `"U"` + U+1D64。
pub const FIELD_U_X: &str = "U\u{1d64}";
/// `"U"` + U+1D65。
pub const FIELD_U_Y: &str = "U\u{1d65}";
/// `"V"` + U+1D64。
pub const FIELD_V_X: &str = "V\u{1d64}";
/// `"V"` + U+1D65。
pub const FIELD_V_Y: &str = "V\u{1d65}";
/// `"X"` + U+1D64。
pub const FIELD_X_X: &str = "X\u{1d64}";
/// `"X"` + U+1D65。
pub const FIELD_X_Y: &str = "X\u{1d65}";
/// `"Y"` + U+1D64。
pub const FIELD_Y_X: &str = "Y\u{1d64}";
/// `"Y"` + U+1D65。
pub const FIELD_Y_Y: &str = "Y\u{1d65}";

/// Go 的 `Pake` 里那些**公开编码中恒为 `null`** 的字段。
///
/// 这是本项目踩过的最贵的一个坑，所以写清楚：
///
/// Go 的 `Pake.Bytes()` 是 `json.Marshal(p.Public())`，而 `Public()` 返回的是一个
/// **完整的 `Pake` 结构体**（只填了 Role/U/V/X/Y，其余留零值）。`Pake` 的字段
/// **没有 `json` tag、也没有 `omitempty`**，所以 `encoding/json` 会把**每一个导出
/// 字段**都编出来——包括那 12 个 `nil` 的私有量，它们全都变成 `null`。
///
/// 这串字节**直接进 PAKE 转录**（`pakekey.Context.Initiator` / `Responder` 就是它），
/// 所以少一个字段、多一个字段、顺序差一点，派生出的密钥就完全不同——表现是
/// "双方都算完了但谁也解不开对方的话"。
///
/// 顺序必须与 Go 的**结构体声明顺序**一致（`encoding/json` 按声明顺序输出）。
/// 未导出字段（`curve` / `curveName` / `idA` / `idB` / `withIDs`）不出现。
/// 注意下标用的是 **U+1D64 / U+1D65（ᵤ / ᵥ）**，不是 α/β：Go 里写的是
/// `Vpwᵤ, Vpwᵥ`、`Upwᵤ, Upwᵥ`、`Zᵤ, Zᵥ`；只有 `Aα` 那一族用 U+03B1。
pub const TRAILING_NULL_FIELDS: [&str; 12] = [
    "P",                 // *big.Int
    "Pw",                // []byte
    "Vpw\u{1d64}",       // *big.Int
    "Vpw\u{1d65}",       // *big.Int
    "Upw\u{1d64}",       // *big.Int
    "Upw\u{1d65}",       // *big.Int
    "A\u{03b1}",         // []byte
    "A\u{03b1}\u{1d64}", // *big.Int
    "A\u{03b1}\u{1d65}", // *big.Int
    "Z\u{1d64}",         // *big.Int
    "Z\u{1d65}",         // *big.Int
    "K",                 // []byte
];

/// `pake.Pake` 的公开部分。
///
/// `x`/`y` 为 `None` 对应 Go 里的 `nil *big.Int`，编码成 `null`。B 方在 `Update`
/// 之前两个都是 `None`；A 方在 `Update` 之前只有 `y` 是 `None`。
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct PublicState {
    /// 0 = A 方，1 = B 方。
    pub role: u8,
    /// 固定点 `U`。
    pub u: AffinePoint,
    /// 固定点 `V`。
    pub v: AffinePoint,
    /// 本端第一段公开值 `X`。
    pub x: Option<AffinePoint>,
    /// 本端第二段公开值 `Y`。
    pub y: Option<AffinePoint>,
}

impl PublicState {
    /// 编码成 Go `json.Marshal` 的等价字符串。
    pub fn encode(&self) -> String {
        let mut out = String::with_capacity(1024);
        out.push_str("{\"");
        out.push_str(FIELD_ROLE);
        out.push_str("\":");
        out.push_str(&self.role.to_string());
        push_field(&mut out, FIELD_U_X, Some(self.u.x()));
        push_field(&mut out, FIELD_U_Y, Some(self.u.y()));
        push_field(&mut out, FIELD_V_X, Some(self.v.x()));
        push_field(&mut out, FIELD_V_Y, Some(self.v.y()));
        push_field(&mut out, FIELD_X_X, self.x.as_ref().map(AffinePoint::x));
        push_field(&mut out, FIELD_X_Y, self.x.as_ref().map(AffinePoint::y));
        push_field(&mut out, FIELD_Y_X, self.y.as_ref().map(AffinePoint::x));
        push_field(&mut out, FIELD_Y_Y, self.y.as_ref().map(AffinePoint::y));
        // Go 的 `Public()` 只填 Role/U/V/X/Y，其余 12 个导出字段是零值——但它们
        // **照样会被编出来**（没有 `omitempty`），而且这些字节要进转录。见上面
        // `TRAILING_NULL_FIELDS` 的注释。
        for name in TRAILING_NULL_FIELDS {
            push_field(&mut out, name, None);
        }
        out.push('}');
        out
    }
}

fn push_field(out: &mut String, name: &str, value: Option<&BigInt>) {
    out.push_str(",\"");
    out.push_str(name);
    out.push_str("\":");
    match value {
        Some(value) => out.push_str(&value.to_string()),
        None => out.push_str("null"),
    }
}

/// 解码一个 `pake.Pake` 公开值。
///
/// 宽容度对齐 Go 的 `json.Unmarshal`：未知字段被忽略，缺失的 `Role` 按 0 处理。
/// 但**必需坐标缺失**（比如 B 方没带 `X`）会返回错误，而不是像 Go 那样在下游 panic。
pub fn decode(bytes: &[u8]) -> Result<PublicState, PakeError> {
    let text = core::str::from_utf8(bytes).map_err(|_| PakeError::MalformedWire)?;
    let mut parser = Parser::new(text);
    let value = parser.parse_value()?;
    parser.skip_whitespace();
    if parser.peek().is_some() {
        return Err(PakeError::MalformedWire);
    }

    Ok(PublicState {
        // Go 的 `Role int` 缺省是 0。
        role: value.field(FIELD_ROLE).and_then(Value::as_u8).unwrap_or(0),
        u: required_point(&value, FIELD_U_X, FIELD_U_Y)?,
        v: required_point(&value, FIELD_V_X, FIELD_V_Y)?,
        x: optional_point(&value, FIELD_X_X, FIELD_X_Y)?,
        y: optional_point(&value, FIELD_Y_X, FIELD_Y_Y)?,
    })
}

fn required_point(object: &Value, x_key: &str, y_key: &str) -> Result<AffinePoint, PakeError> {
    let x = object.field(x_key).and_then(Value::as_bigint).ok_or(PakeError::MalformedWire)?;
    let y = object.field(y_key).and_then(Value::as_bigint).ok_or(PakeError::MalformedWire)?;
    Ok(AffinePoint::new(x, y))
}

fn optional_point(object: &Value, x_key: &str, y_key: &str) -> Result<Option<AffinePoint>, PakeError> {
    let absent = |entry: Option<&Value>| matches!(entry, None | Some(Value::Null));
    if absent(object.field(x_key)) && absent(object.field(y_key)) {
        return Ok(None);
    }
    required_point(object, x_key, y_key).map(Some)
}

/// 一个只够用的 JSON 值模型。
///
/// 数字按**原始字面量**保存：croc 的坐标是 150 位十进制整数，而我们只对「能被解析成
/// 整数」的字段感兴趣，存字符串就不需要在解析阶段区分整数与浮点。
#[derive(Clone, Debug, PartialEq, Eq)]
enum Value {
    Null,
    Bool(bool),
    Number(String),
    String(String),
    Array(Vec<Self>),
    Object(Vec<(String, Self)>),
}

impl Value {
    fn field(&self, name: &str) -> Option<&Self> {
        match self {
            Self::Object(entries) => entries.iter().find(|(key, _)| key == name).map(|(_, value)| value),
            _ => None,
        }
    }

    fn as_bigint(&self) -> Option<BigInt> {
        match self {
            Self::Number(literal) => parse_int(literal),
            _ => None,
        }
    }

    fn as_u8(&self) -> Option<u8> {
        self.as_bigint().and_then(|value| value.to_u8())
    }
}

/// 字节级 JSON 读取器。
///
/// 用字节游标而不是 `char` 迭代器：字符串里的多字节 UTF-8 直接按字节收集，最后一次性
/// `from_utf8` 校验，既简单又不会把码点拆坏。
struct Parser<'a> {
    bytes: &'a [u8],
    position: usize,
}

impl<'a> Parser<'a> {
    fn new(text: &'a str) -> Self {
        Self { bytes: text.as_bytes(), position: 0 }
    }

    fn peek(&self) -> Option<u8> {
        self.bytes.get(self.position).copied()
    }

    fn bump(&mut self) -> Option<u8> {
        let byte = self.peek()?;
        self.position += 1;
        Some(byte)
    }

    fn skip_whitespace(&mut self) {
        while matches!(self.peek(), Some(b' ' | b'\t' | b'\n' | b'\r')) {
            self.position += 1;
        }
    }

    fn expect(&mut self, expected: u8) -> Result<(), PakeError> {
        match self.bump() {
            Some(byte) if byte == expected => Ok(()),
            _ => Err(PakeError::MalformedWire),
        }
    }

    fn consume_literal(&mut self, literal: &str) -> Result<(), PakeError> {
        let rest = self.bytes.get(self.position..).ok_or(PakeError::MalformedWire)?;
        if !rest.starts_with(literal.as_bytes()) {
            return Err(PakeError::MalformedWire);
        }
        self.position += literal.len();
        Ok(())
    }

    fn parse_value(&mut self) -> Result<Value, PakeError> {
        self.skip_whitespace();
        match self.peek() {
            Some(b'n') => self.consume_literal("null").map(|()| Value::Null),
            Some(b't') => self.consume_literal("true").map(|()| Value::Bool(true)),
            Some(b'f') => self.consume_literal("false").map(|()| Value::Bool(false)),
            Some(b'"') => self.parse_string().map(Value::String),
            Some(b'[') => self.parse_array(),
            Some(b'{') => self.parse_object(),
            Some(b'-' | b'0'..=b'9') => self.parse_number(),
            _ => Err(PakeError::MalformedWire),
        }
    }

    fn parse_array(&mut self) -> Result<Value, PakeError> {
        self.expect(b'[')?;
        let mut items = Vec::new();
        self.skip_whitespace();
        if self.peek() == Some(b']') {
            self.bump();
            return Ok(Value::Array(items));
        }
        loop {
            items.push(self.parse_value()?);
            self.skip_whitespace();
            match self.bump() {
                Some(b',') => {}
                Some(b']') => return Ok(Value::Array(items)),
                _ => return Err(PakeError::MalformedWire),
            }
        }
    }

    fn parse_object(&mut self) -> Result<Value, PakeError> {
        self.expect(b'{')?;
        let mut entries = Vec::new();
        self.skip_whitespace();
        if self.peek() == Some(b'}') {
            self.bump();
            return Ok(Value::Object(entries));
        }
        loop {
            self.skip_whitespace();
            let key = self.parse_string()?;
            self.skip_whitespace();
            self.expect(b':')?;
            let value = self.parse_value()?;
            entries.push((key, value));
            self.skip_whitespace();
            match self.bump() {
                Some(b',') => {}
                Some(b'}') => return Ok(Value::Object(entries)),
                _ => return Err(PakeError::MalformedWire),
            }
        }
    }

    fn parse_string(&mut self) -> Result<String, PakeError> {
        self.expect(b'"')?;
        let mut out: Vec<u8> = Vec::new();
        loop {
            match self.bump() {
                Some(b'"') => return String::from_utf8(out).map_err(|_| PakeError::MalformedWire),
                Some(b'\\') => self.parse_escape(&mut out)?,
                Some(byte) if byte < 0x20 => return Err(PakeError::MalformedWire),
                Some(byte) => out.push(byte),
                None => return Err(PakeError::MalformedWire),
            }
        }
    }

    fn parse_escape(&mut self, out: &mut Vec<u8>) -> Result<(), PakeError> {
        match self.bump() {
            Some(b'"') => out.push(b'"'),
            Some(b'\\') => out.push(b'\\'),
            Some(b'/') => out.push(b'/'),
            Some(b'b') => out.push(0x08),
            Some(b'f') => out.push(0x0c),
            Some(b'n') => out.push(b'\n'),
            Some(b'r') => out.push(b'\r'),
            Some(b't') => out.push(b'\t'),
            Some(b'u') => {
                // 只处理 BMP 内的单个转义；代理对（`\ud83d\ude00`）不会出现在 croc 的
                // 线路值里，遇到就报错而不是猜。
                let code = self.parse_hex4()?;
                let character = char::from_u32(code).ok_or(PakeError::MalformedWire)?;
                let mut buffer = [0u8; 4];
                out.extend_from_slice(character.encode_utf8(&mut buffer).as_bytes());
            }
            _ => return Err(PakeError::MalformedWire),
        }
        Ok(())
    }

    fn parse_hex4(&mut self) -> Result<u32, PakeError> {
        let mut value = 0u32;
        for _ in 0..4 {
            let digit = self.bump().and_then(hex_digit).ok_or(PakeError::MalformedWire)?;
            value = value * 16 + digit;
        }
        Ok(value)
    }

    fn parse_number(&mut self) -> Result<Value, PakeError> {
        let start = self.position;
        if self.peek() == Some(b'-') {
            self.position += 1;
        }
        while matches!(self.peek(), Some(b'0'..=b'9' | b'.' | b'e' | b'E' | b'+' | b'-')) {
            self.position += 1;
        }
        let literal = self.bytes.get(start..self.position).ok_or(PakeError::MalformedWire)?;
        let literal = core::str::from_utf8(literal).map_err(|_| PakeError::MalformedWire)?;
        if literal.is_empty() {
            return Err(PakeError::MalformedWire);
        }
        Ok(Value::Number(literal.to_owned()))
    }
}

fn hex_digit(byte: u8) -> Option<u32> {
    match byte {
        b'0'..=b'9' => Some(u32::from(byte - b'0')),
        b'a'..=b'f' => Some(u32::from(byte - b'a') + 10),
        b'A'..=b'F' => Some(u32::from(byte - b'A') + 10),
        _ => None,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn point(x: i64, y: i64) -> AffinePoint {
        AffinePoint::new(BigInt::from(x), BigInt::from(y))
    }

    fn sample() -> PublicState {
        PublicState { role: 0, u: point(5, 12), v: point(7, 8), x: Some(point(1, 2)), y: None }
    }

    #[test]
    fn field_names_use_the_expected_code_points() {
        assert_eq!(FIELD_U_X, "U\u{1d64}");
        assert_eq!(FIELD_U_Y, "U\u{1d65}");
        assert_eq!(FIELD_V_X, "V\u{1d64}");
        assert_eq!(FIELD_V_Y, "V\u{1d65}");
        assert_eq!(FIELD_X_X, "X\u{1d64}");
        assert_eq!(FIELD_X_Y, "X\u{1d65}");
        assert_eq!(FIELD_Y_X, "Y\u{1d64}");
        assert_eq!(FIELD_Y_Y, "Y\u{1d65}");
    }

    #[test]
    fn the_trailing_null_fields_are_always_emitted() {
        // Go 的 `Public()` 返回整个 `Pake` 结构体，`json.Marshal` 又没有 `omitempty`，
        // 于是 21 个导出字段全都会出现。这些字节**进 PAKE 转录**，少一个都不行。
        let state = PublicState {
            role: 0,
            u: AffinePoint::new(BigInt::from(1u32), BigInt::from(2u32)),
            v: AffinePoint::new(BigInt::from(3u32), BigInt::from(4u32)),
            x: None,
            y: None,
        };
        let json = state.encode();
        // 字面量写死（而不是从 TRAILING_NULL_FIELDS 拼），这样常量写错时这条会红。
        let expected_tail = concat!(
            ",\"P\":null,\"Pw\":null,",
            "\"Vpw\u{1d64}\":null,\"Vpw\u{1d65}\":null,",
            "\"Upw\u{1d64}\":null,\"Upw\u{1d65}\":null,",
            "\"A\u{03b1}\":null,\"A\u{03b1}\u{1d64}\":null,\"A\u{03b1}\u{1d65}\":null,",
            "\"Z\u{1d64}\":null,\"Z\u{1d65}\":null,",
            "\"K\":null}"
        );
        assert!(json.ends_with(expected_tail), "{json}");
        assert_eq!(json.matches(':').count(), 21, "Go 的导出字段恰好 21 个：{json}");
    }

    #[test]
    fn encoding_matches_go_field_order_and_names() {
        // 逐字节钉住：9 个有值的字段在前，然后 12 个恒为 `null` 的尾随字段。
        // Go 那边是 `json.Marshal(p.Public())`，`Pake` 没有 `omitempty`。
        let mut expected = format!(
            "{{\"Role\":0,\"{}\":5,\"{}\":12,\"{}\":7,\"{}\":8,\"{}\":1,\"{}\":2,\"{}\":null,\"{}\":null",
            FIELD_U_X, FIELD_U_Y, FIELD_V_X, FIELD_V_Y, FIELD_X_X, FIELD_X_Y, FIELD_Y_X, FIELD_Y_Y
        );
        for name in TRAILING_NULL_FIELDS {
            expected.push_str(&format!(",\"{name}\":null"));
        }
        expected.push('}');
        assert_eq!(sample().encode(), expected);
    }

    #[test]
    fn encoding_uses_bare_decimal_numbers() {
        let encoded = sample().encode();
        assert!(encoded.contains(&format!("\"{FIELD_U_X}\":5,")));
        assert!(!encoded.contains(&format!("\"{FIELD_U_X}\":\"5\"")));
        assert!(encoded.contains(&format!("\"{FIELD_Y_X}\":null")));
    }

    #[test]
    fn encoding_round_trips_through_decode() {
        let original = sample();
        let decoded = decode(original.encode().as_bytes()).expect("自编码应能解回");
        assert_eq!(decoded, original);
    }

    #[test]
    fn decoding_tolerates_unknown_fields_and_whitespace() {
        let text = format!(
            " {{ \"Extra\" : [1, {{\"a\": null}}], \"Role\" : 1 , \"{}\" : 5 , \"{}\" : 12 , \
             \"{}\" : 7 , \"{}\" : 8 , \"{}\" : 1 , \"{}\" : 2 , \"{}\" : 3 , \"{}\" : 4 }} ",
            FIELD_U_X, FIELD_U_Y, FIELD_V_X, FIELD_V_Y, FIELD_X_X, FIELD_X_Y, FIELD_Y_X, FIELD_Y_Y
        );
        let decoded = decode(text.as_bytes()).expect("应忽略未知字段");
        assert_eq!(decoded.role, 1);
        assert_eq!(decoded.x, Some(point(1, 2)));
        assert_eq!(decoded.y, Some(point(3, 4)));
    }

    #[test]
    fn decoding_defaults_missing_role_to_zero() {
        let text = format!("{{\"{}\":5,\"{}\":12,\"{}\":7,\"{}\":8}}", FIELD_U_X, FIELD_U_Y, FIELD_V_X, FIELD_V_Y);
        let decoded = decode(text.as_bytes()).expect("缺失 Role 时按 Go 语义取 0");
        assert_eq!(decoded.role, 0);
        assert_eq!(decoded.x, None);
    }

    #[test]
    fn decoding_accepts_escaped_keys() {
        // Go 的 `json.Marshal` 会把 `<`、`>`、`&` 转义，但我们的字段名不含这些字符。
        // 这里验证转义解析本身可用：`\u0055` 就是 `U`。
        let text = format!("{{\"\\u0055\\u1d64\":5,\"{}\":12,\"{}\":7,\"{}\":8}}", FIELD_U_Y, FIELD_V_X, FIELD_V_Y);
        let decoded = decode(text.as_bytes()).expect("应能解析 \\u 转义");
        assert_eq!(decoded.u, point(5, 12));
    }

    #[test]
    fn decoding_rejects_malformed_input() {
        for text in ["", "{", "[]", "not json", "{\"U\":}", "{\"U\":5,}"] {
            assert!(decode(text.as_bytes()).is_err(), "{text:?} 应被拒绝");
        }
        // 缺少 `V` 的公开值不完整。
        let missing_v = format!("{{\"{}\":5,\"{}\":12}}", FIELD_U_X, FIELD_U_Y);
        assert!(decode(missing_v.as_bytes()).is_err());
    }

    #[test]
    fn decoding_rejects_non_integer_coordinates() {
        let text = format!("{{\"{}\":1.5,\"{}\":12,\"{}\":7,\"{}\":8}}", FIELD_U_X, FIELD_U_Y, FIELD_V_X, FIELD_V_Y);
        assert!(decode(text.as_bytes()).is_err());
    }

    #[test]
    fn parsing_handles_nested_structures_and_escapes() {
        let mut parser = Parser::new("{\"a\":[\"\\n\",true,null,-12],\"b\":{}}");
        let value = parser.parse_value().expect("应能解析嵌套结构");
        assert_eq!(value.field("a").and_then(Value::as_bigint), None);
        assert!(value.field("b").is_some());
    }

    #[test]
    fn parsing_rejects_trailing_garbage() {
        let text =
            format!("{{\"{}\":5,\"{}\":12,\"{}\":7,\"{}\":8}}trailing", FIELD_U_X, FIELD_U_Y, FIELD_V_X, FIELD_V_Y);
        assert!(decode(text.as_bytes()).is_err());
    }

    #[test]
    fn negative_numbers_round_trip_through_the_parser() {
        // Go 的 big.Int 可能为负（例如 `Neg` 之后没取模），所以解析器必须认负号。
        assert_eq!(parse_int("-42"), Some(BigInt::from(-42)));
        let mut parser = Parser::new("{\"n\":-42}");
        let value = parser.parse_value().expect("应能解析负数");
        assert_eq!(value.field("n").and_then(Value::as_bigint), Some(BigInt::from(-42)));
    }
}
