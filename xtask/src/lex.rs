//! 源码里"像代码的部分"提取。
//!
//! 纯粹按文本扫关键词会误报：`croc-protocol` 的文档注释里就写着"没有 `tokio`、没有
//! `std::fs`"。所以先把注释和字符串字面量抹掉，再在剩下的代码里找关键词。
//!
//! 这是个够用就好的词法器：它不解析 Rust，只按语法形状跳过注释、普通字符串、原始
//! 字符串和字符字面量。故意保留所有非 ASCII 字节，避免把 UTF-8 拆坏。

/// 把注释与字符串字面量替换成空格，返回剩下的代码字节。
///
/// 长度与偏移和输入完全一致：被抹掉的字节逐个换成空格，只有换行原样保留。这样
/// 命中位置可以直接换算出准确的行号。
#[must_use]
pub fn strip_non_code(source: &str) -> Vec<u8> {
    let bytes = source.as_bytes();
    let mut code: Vec<u8> = Vec::with_capacity(bytes.len());
    let mut index = 0usize;

    while index < bytes.len() {
        let end = if starts_with_at(bytes, index, b"//") {
            skip_line_comment(bytes, index)
        } else if starts_with_at(bytes, index, b"/*") {
            skip_block_comment(bytes, index)
        } else if let Some(width) = raw_string_width(bytes, index) {
            index + width
        } else if bytes.get(index) == Some(&b'"') {
            skip_quoted(bytes, index, b'"')
        } else if bytes.get(index) == Some(&b'\'') && is_char_literal(bytes, index) {
            skip_quoted(bytes, index, b'\'')
        } else {
            match bytes.get(index) {
                Some(byte) => code.push(*byte),
                None => break,
            }
            index += 1;
            continue;
        };
        // 保留换行，其余抹成空格——长度不变，行号也就不会漂。
        for byte in bytes.get(index..end).unwrap_or_default() {
            code.push(if *byte == b'\n' { b'\n' } else { b' ' });
        }
        index = end;
    }
    code
}

fn starts_with_at(bytes: &[u8], index: usize, pattern: &[u8]) -> bool {
    bytes.get(index..).is_some_and(|rest| rest.starts_with(pattern))
}

/// 跳到行尾之后（保留换行，方便行号统计）。
fn skip_line_comment(bytes: &[u8], index: usize) -> usize {
    let mut cursor = index;
    while let Some(byte) = bytes.get(cursor) {
        cursor += 1;
        if *byte == b'\n' {
            break;
        }
    }
    cursor
}

/// 跳过 `/* … */`。Rust 的块注释可以嵌套。
fn skip_block_comment(bytes: &[u8], index: usize) -> usize {
    let mut cursor = index + 2;
    let mut depth = 1usize;
    while depth > 0 {
        if starts_with_at(bytes, cursor, b"/*") {
            depth += 1;
            cursor += 2;
        } else if starts_with_at(bytes, cursor, b"*/") {
            depth -= 1;
            cursor += 2;
        } else if cursor < bytes.len() {
            cursor += 1;
        } else {
            break;
        }
    }
    cursor
}

/// 跳过一段引号包裹的内容，返回收尾引号之后的偏移。处理 `\` 转义。
fn skip_quoted(bytes: &[u8], index: usize, quote: u8) -> usize {
    let mut cursor = index + 1;
    while let Some(byte) = bytes.get(cursor) {
        if *byte == b'\\' {
            cursor += 2;
            continue;
        }
        cursor += 1;
        if *byte == quote {
            break;
        }
    }
    cursor
}

/// 若 `index` 处是一个原始字符串字面量（`r"…"`、`r#"…"#`、`br#"…"#`），返回它的总长度。
fn raw_string_width(bytes: &[u8], index: usize) -> Option<usize> {
    let mut cursor = index;
    if bytes.get(cursor) == Some(&b'b') {
        cursor += 1;
    }
    if bytes.get(cursor) != Some(&b'r') {
        return None;
    }
    cursor += 1;
    let hashes_start = cursor;
    while bytes.get(cursor) == Some(&b'#') {
        cursor += 1;
    }
    let hashes = cursor - hashes_start;
    if bytes.get(cursor) != Some(&b'"') {
        return None;
    }
    cursor += 1;

    // 收尾是 `"` 加同样数量的 `#`。
    while let Some(byte) = bytes.get(cursor) {
        if *byte == b'"'
            && bytes.get(cursor + 1..cursor + 1 + hashes).is_some_and(|tail| tail.iter().all(|b| *b == b'#'))
        {
            return Some(cursor + 1 + hashes - index);
        }
        cursor += 1;
    }
    Some(bytes.len() - index)
}

/// `index` 处的 `'` 是字符字面量的开始，还是生命周期标注的开始。
fn is_char_literal(bytes: &[u8], index: usize) -> bool {
    match bytes.get(index + 1) {
        None => false,
        // `'\n'`、`'\''` 之类：紧跟反斜杠的一定是字面量。
        Some(b'\\') => true,
        Some(&first) => bytes.get(index + 1 + utf8_char_width(first)) == Some(&b'\''),
    }
}

/// 一个 UTF-8 首字节对应的字符宽度。非法首字节按 1 处理。
const fn utf8_char_width(byte: u8) -> usize {
    if byte < 0x80 {
        1
    } else if byte >= 0xf0 {
        4
    } else if byte >= 0xe0 {
        3
    } else if byte >= 0xc0 {
        2
    } else {
        1
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn stripped(source: &str) -> String {
        String::from_utf8_lossy(&strip_non_code(source)).into_owned()
    }

    /// 代码里是否出现了 `needle`（纯字节比较）。
    fn contains(code: &[u8], needle: &str) -> bool {
        let needle = needle.as_bytes();
        !needle.is_empty() && needle.len() <= code.len() && code.windows(needle.len()).any(|window| window == needle)
    }

    #[test]
    fn line_comments_are_removed_but_the_newline_stays() {
        let code = stripped("let a = 1; // tokio::spawn\nlet b = 2;");
        assert!(!contains(code.as_bytes(), "tokio::"));
        assert!(contains(code.as_bytes(), "let b = 2;"));
        assert_eq!(code.matches('\n').count(), 1, "换行要保留，行号才对得上");
    }

    #[test]
    fn block_comments_are_removed_and_nest() {
        let code = stripped("a /* std::fs /* nested */ still */ b");
        assert!(!contains(code.as_bytes(), "std::fs"));
        assert!(contains(code.as_bytes(), "a "));
        assert!(contains(code.as_bytes(), " b"));
    }

    #[test]
    fn doc_comments_do_not_trigger_false_positives() {
        // 这正是真实场景：croc-protocol 的文档里就写着这些词。
        let code = stripped("//! 没有 `tokio`、没有 `std::fs`、没有 `Instant::now`。\nfn pure() {}");
        assert!(!contains(code.as_bytes(), "tokio"));
        assert!(!contains(code.as_bytes(), "std::fs"));
        assert!(!contains(code.as_bytes(), "Instant::now"));
        assert!(contains(code.as_bytes(), "fn pure()"));
    }

    #[test]
    fn string_literals_are_removed() {
        let code = stripped(r#"let hint = "use std::fs::File";"#);
        assert!(!contains(code.as_bytes(), "std::fs"));
        assert!(contains(code.as_bytes(), "let hint ="));
    }

    #[test]
    fn escaped_quotes_do_not_end_a_string_early() {
        let code = stripped(r#"let s = "a\"b tokio::"; let after = 1;"#);
        assert!(!contains(code.as_bytes(), "tokio::"));
        assert!(contains(code.as_bytes(), "let after = 1;"));
    }

    #[test]
    fn raw_strings_are_removed() {
        let code = stripped(r##"let a = r#"tokio::spawn"#; let b = br#"std::fs"#;"##);
        assert!(!contains(code.as_bytes(), "tokio::"));
        assert!(!contains(code.as_bytes(), "std::fs"));
        assert!(contains(code.as_bytes(), "let b ="));
    }

    #[test]
    fn char_literals_are_removed_but_lifetimes_are_not() {
        let code = stripped("fn f<'a>(x: &'a str) -> char { 'x' }");
        assert!(contains(code.as_bytes(), "<'a>"), "生命周期不是字符字面量");
        assert!(contains(code.as_bytes(), "&'a str"));
        assert!(!contains(code.as_bytes(), "'x'"), "字符字面量要被抹掉");
    }

    #[test]
    fn multibyte_char_literals_are_handled() {
        let code = stripped("let c = '中'; let d = 1;");
        assert!(!contains(code.as_bytes(), "'中'"));
        assert!(contains(code.as_bytes(), "let d = 1;"), "多字节字符不该把后面的代码吃掉：{code}");
    }

    #[test]
    fn real_code_survives_the_strip() {
        let code = stripped("fn main() { tokio::spawn(async {}); }");
        assert!(contains(code.as_bytes(), "tokio::spawn"), "真正的调用必须留下");
    }

    #[test]
    fn an_unterminated_block_comment_swallows_the_rest() {
        // 语法错误的源码不该让扫描器 panic，只是什么都找不到。
        assert_eq!(stripped("a /* never closed").trim_end(), "a");
    }

    #[test]
    fn stripping_preserves_length_and_line_breaks() {
        let source = "// tokio\nlet a = \"std::fs\";\n/* Instant::now */\n";
        let code = strip_non_code(source);
        assert_eq!(code.len(), source.len(), "长度必须一致，否则行号会漂");
        assert_eq!(code.iter().filter(|byte| **byte == b'\n').count(), 3);
        // 行号可以按换行数直接算。
        let line =
            |offset: usize| code.get(..offset).map_or(0, |head| head.iter().filter(|b| **b == b'\n').count()) + 1;
        assert_eq!(line(source.find("Instant::now").unwrap_or(0)), 3);
    }

    #[test]
    fn contains_handles_edge_inputs() {
        assert!(!contains(b"", "x"));
        assert!(!contains(b"abc", "abcd"));
        assert!(!contains(b"abc", ""));
        assert!(contains(b"abc", "bc"));
    }
}
