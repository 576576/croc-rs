//! 分块规划与断点续传的区间运算。
//!
//! 全部是纯函数：不读文件、不看时钟。文件那一步（"这一块是不是还是全零"）由
//! `croc-engine` 做完之后把判定结果喂进来，见 [`missing_ranges`]。
//!
//! # 线路形态
//!
//! 接收方告诉发送方"我要哪些块"，用的是 Go 的 `[]int64`，布局是**扁平的三元组序列**：
//!
//! ```text
//! [chunkSize, start0, count0, start1, count1, …]
//!  ─────────  ─────────────  ─────────────
//!   块大小     第 0 段          第 1 段
//! ```
//!
//! - `start` 是**字节偏移**（不是块序号），`count` 是**块数**（不是字节数）；
//! - `chunkSize` 只在列表非空时出现一次；
//! - **空列表表示"整个文件"**——这是最容易踩的坑，见 [`ChunkRanges::is_whole`]。

use std::ops::Range;

/// croc 的数据块大小。
///
/// Go 侧写作 `models.TCP_BUFFER_SIZE / 2`（64 KiB / 2）。这个值同时决定接收方怎么扫
/// 断点、发送方怎么切块，两边必须一致。
pub const CHUNK_SIZE: u64 = 32 * 1024;

/// [`CHUNK_SIZE`] 的 `i64` 形态，供线路列表使用。
const CHUNK_SIZE_I64: i64 = 32 * 1024;

/// 一个半开字节区间 `[start, end)`。
pub type ByteRange = Range<u64>;

/// 覆盖 `file_size` 个字节需要多少个块。
///
/// 最后一个块可能不满，但仍算一个。
#[must_use]
pub fn chunk_count(file_size: u64, chunk_size: u64) -> u64 {
    if chunk_size == 0 {
        return 0;
    }
    file_size.div_ceil(chunk_size)
}

/// 把 `size` 个字节切成 `chunk_size` 大小的块区间，惰性生成。
///
/// 输入为 0 时产出空迭代器——调用方不必特判。
pub fn plan(size: u64, chunk_size: u64) -> impl Iterator<Item = ByteRange> {
    (0..chunk_count(size, chunk_size)).map(move |index| {
        let start = index.saturating_mul(chunk_size);
        start..start.saturating_add(chunk_size).min(size)
    })
}

/// 一块数据是否全零。
///
/// croc 的断点续传靠这个：接收方先把文件 `truncate` 到完整大小，已收到的部分写在
/// 正确偏移上，没收到的部分保持全零。于是"扫一遍零块"就等于"扫一遍缺失块"，不需要
/// 额外的状态文件。
#[must_use]
pub fn is_all_zero(bytes: &[u8]) -> bool {
    bytes.iter().all(|byte| *byte == 0)
}

/// 接收方"要哪些块"的请求。
///
/// 内部就是线路形态的 `Vec<i64>`，没有做任何归一化——`croc-protocol` 直接把它塞进
/// `RemoteFileRequest.CurrentFileChunkRanges`。之所以包一层，是为了让
/// [`is_whole`](Self::is_whole) 这个反直觉的语义有个能被读到的名字。
#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub struct ChunkRanges(Vec<i64>);

impl ChunkRanges {
    /// 空列表：整个文件。
    #[must_use]
    pub const fn whole() -> Self {
        Self(Vec::new())
    }

    /// 从线路形态构造。
    #[must_use]
    pub const fn from_wire(wire: Vec<i64>) -> Self {
        Self(wire)
    }

    /// 线路形态。
    #[must_use]
    pub fn as_wire(&self) -> &[i64] {
        &self.0
    }

    /// 取出线路形态。
    #[must_use]
    pub fn into_wire(self) -> Vec<i64> {
        self.0
    }

    /// 是否表示"整个文件"。
    ///
    /// 注意这也包括"一块都不缺"的情况——[`missing_ranges`] 对一个已经收完的文件同样
    /// 返回空列表，于是发送方会把整个文件再传一遍。这是 Go 的既有行为：接收方的
    /// 判定无法区分"全都在"和"全都不在"，而两种情况下重传都是安全的。保持一致比
    /// 自己发明一套语义更重要。
    #[must_use]
    pub fn is_whole(&self) -> bool {
        self.0.is_empty()
    }

    /// 声明的块大小。空列表或声明值非正时返回 `None`。
    #[must_use]
    pub fn chunk_size(&self) -> Option<u64> {
        self.0.first().and_then(|value| u64::try_from(*value).ok()).filter(|size| *size > 0)
    }

    /// 请求的块数。
    ///
    /// 空列表时按 `CHUNK_SIZE` 折算整个文件。
    #[must_use]
    pub fn count(&self, file_size: u64) -> u64 {
        if file_size == 0 {
            return 0;
        }
        if self.is_whole() {
            return chunk_count(file_size, CHUNK_SIZE);
        }
        self.pairs().filter_map(|(_, count)| u64::try_from(count).ok()).filter(|count| *count > 0).sum()
    }

    /// 请求的字节数，含最后一个可能不满的块。
    ///
    /// 空列表时就是整个文件。
    #[must_use]
    pub fn bytes(&self, file_size: u64) -> u64 {
        if file_size == 0 {
            return 0;
        }
        if self.is_whole() {
            return file_size;
        }
        let chunk_size = self.chunk_size().unwrap_or(CHUNK_SIZE);
        self.pairs().filter_map(|(start, count)| range_bytes(start, count, chunk_size, file_size)).sum()
    }

    /// 第 `position` 个字节所在的块是否被请求。空列表请求所有块。
    ///
    /// 区间必须按 `start` 升序——[`missing_ranges`] 保证这一点。用二分而不是线性扫描，
    /// 是因为一个高度碎片化的文件可能有成百上千段。
    #[must_use]
    pub fn contains(&self, position: u64) -> bool {
        if self.is_whole() {
            return true;
        }
        let Some(chunk_size) = self.chunk_size() else {
            return false;
        };
        let Some(position) = i64::try_from(position).ok() else {
            return false;
        };
        // 找最后一个 `start <= position` 的区间。
        let pair_count = self.0.len().saturating_sub(1) / 2;
        let mut low = 0usize;
        let mut high = pair_count;
        while low < high {
            let mid = low + (high - low) / 2;
            let start = self.0.get(1 + mid * 2).copied().unwrap_or(i64::MAX);
            if start <= position {
                low = mid + 1;
            } else {
                high = mid;
            }
        }
        if low == 0 {
            return false;
        }
        let index = 1 + (low - 1) * 2;
        let start = self.0.get(index).copied().unwrap_or_default();
        let count = self.0.get(index + 1).copied().unwrap_or_default();
        count > 0 && position < start.saturating_add(count.saturating_mul(to_wire(chunk_size)))
    }

    /// 惰性展开每个被请求块的字节偏移。
    ///
    /// 这是 Go `ChunkRangesToChunks` 的迭代器版本——发送方的 worker 队列就是在这个
    /// 序列上依次 `claim()`，所以不需要先把整张表展开成 `Vec`。
    pub fn chunk_offsets(&self) -> impl Iterator<Item = u64> + '_ {
        let chunk_size = self.chunk_size().unwrap_or(CHUNK_SIZE);
        self.pairs().filter(|(_, count)| *count > 0).flat_map(move |(start, count)| {
            let start = u64::try_from(start).unwrap_or(0);
            let count = u64::try_from(count).unwrap_or(0);
            (0..count).filter_map(move |index| index.checked_mul(chunk_size).and_then(|delta| start.checked_add(delta)))
        })
    }

    /// 把"整个文件"展开成显式的覆盖区间。
    ///
    /// 发送方的 chunk 队列需要一段可遍历的区间列表，而空列表给不出偏移。Go 在
    /// `newRequestedChunkQueue` 里做同样的替换。
    #[must_use]
    pub fn materialize(&self, file_size: u64) -> Self {
        if !self.is_whole() {
            return self.clone();
        }
        Self(vec![CHUNK_SIZE_I64, 0, to_wire(chunk_count(file_size, CHUNK_SIZE))])
    }

    /// 遍历 `(start, count)` 对。
    ///
    /// 长度是偶数时末尾那个孤零零的 `start` 会被丢掉。Go 的 `ChunkRangesToChunks`
    /// 在这种输入上会越界 panic——对方送来的列表长度不由我们控制，所以这里选择丢弃
    /// 而不是崩掉。
    fn pairs(&self) -> impl Iterator<Item = (i64, i64)> + '_ {
        self.0.iter().skip(1).step_by(2).copied().zip(self.0.iter().skip(2).step_by(2).copied())
    }
}

/// 从"每块是否还是全零"构造缺失区间。
///
/// 这是 Go `utils.MissingChunks` 的纯核心：文件扫描那部分（`os.Open` / `io.ReadFull`）
/// 归 `croc-engine`，这里只做 run-length 折叠。
///
/// `chunk_size` 同时决定块大小和 `start` 的字节步长。
///
/// # 那个"什么都没缺"的坑
///
/// 一块都不缺时返回**空列表**，而空列表在线路上表示"整个文件"——于是发送方会把整个
/// 文件再传一遍。这是 Go 的既有行为：接收方的判定无法区分"全都在"和"压根没扫到零块"，
/// 而两种情况下重传都是安全的。整个文件都缺时反而会得到一个覆盖全文件的**显式区间**。
#[must_use]
pub fn missing_ranges(is_zero: impl IntoIterator<Item = bool>, chunk_size: u64) -> ChunkRanges {
    let mut wire: Vec<i64> = Vec::new();
    let mut run_start = 0u64;
    let mut run_count = 0u64;
    for (index, zero) in is_zero.into_iter().enumerate() {
        if !zero {
            push_run(&mut wire, chunk_size, run_start, &mut run_count);
            continue;
        }
        if run_count == 0 {
            run_start = (index as u64).saturating_mul(chunk_size);
        }
        run_count += 1;
    }
    push_run(&mut wire, chunk_size, run_start, &mut run_count);
    ChunkRanges(wire)
}

/// 追加一段 `(start, count)`。`count` 为 0 时什么也不做。
///
/// `chunkSize` 只在列表从空变非空时写一次——这正是线路格式的第一个元素。
fn push_run(wire: &mut Vec<i64>, chunk_size: u64, run_start: u64, run_count: &mut u64) {
    if *run_count == 0 {
        return;
    }
    if wire.is_empty() {
        wire.push(to_wire(chunk_size));
    }
    wire.push(to_wire(run_start));
    wire.push(to_wire(*run_count));
    *run_count = 0;
}

/// 一个区间覆盖了多少字节，落在 `[0, file_size)` 之外的部分被裁掉。
fn range_bytes(start: i64, count: i64, chunk_size: u64, file_size: u64) -> Option<u64> {
    let start = u64::try_from(start).ok()?;
    let count = u64::try_from(count).ok()?;
    if count == 0 || start >= file_size {
        return None;
    }
    let end = start.checked_add(count.checked_mul(chunk_size)?)?.min(file_size);
    (end > start).then_some(end - start)
}

/// `u64` → `i64`，供线路列表使用。
///
/// 线路格式是 Go 的 `[]int64`，所以非负值必须塞得进 `i64`。文件偏移超过 `i64::MAX`
/// （8 EiB）不可能发生；真发生了就饱和——Go 那边是溢出回绕，两者都是"永远不会走到"
/// 的分支，但饱和至少不会算出负数。
fn to_wire(value: u64) -> i64 {
    i64::try_from(value).unwrap_or(i64::MAX)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn chunk_size_matches_the_go_constant() {
        // Go: `models.TCP_BUFFER_SIZE / 2` = 65536 / 2。
        assert_eq!(CHUNK_SIZE, 65536 / 2);
        assert_eq!(u64::try_from(CHUNK_SIZE_I64), Ok(CHUNK_SIZE));
    }

    #[test]
    fn chunk_count_rounds_up() {
        assert_eq!(chunk_count(0, 100), 0);
        assert_eq!(chunk_count(1, 100), 1);
        assert_eq!(chunk_count(100, 100), 1);
        assert_eq!(chunk_count(101, 100), 2);
        assert_eq!(chunk_count(200, 100), 2);
        assert_eq!(chunk_count(100, 0), 0, "零块大小不该除零");
    }

    #[test]
    fn plan_covers_the_file_exactly_once() {
        let ranges: Vec<ByteRange> = plan(250, 100).collect();
        assert_eq!(ranges, vec![0..100, 100..200, 200..250]);
        assert!(plan(0, 100).next().is_none());
        // 区间首尾相接、并集恰好是 [0, size)。
        for size in [1u64, 99, 100, 101, 32768, 32769, 100_000] {
            let ranges: Vec<ByteRange> = plan(size, CHUNK_SIZE).collect();
            let covered: u64 = ranges.iter().map(|range| range.end - range.start).sum();
            assert_eq!(covered, size, "size={size}");
            assert_eq!(ranges.first().map(|range| range.start), Some(0));
            assert_eq!(ranges.last().map(|range| range.end), Some(size));
        }
    }

    #[test]
    fn missing_ranges_writes_the_flattened_triple_layout() {
        // 第 0、1 块全零，第 2 块有数据，第 3、4 块全零。
        let ranges = missing_ranges([true, true, false, true, true], 100);
        assert_eq!(ranges.as_wire(), &[100, 0, 2, 300, 2]);
    }

    #[test]
    fn missing_ranges_uses_byte_offsets_not_chunk_indices() {
        // 第 3 块开始缺失：start 必须是 3 * chunkSize，而不是 3。
        let ranges = missing_ranges([false, false, false, true], 50);
        assert_eq!(ranges.as_wire(), &[50, 150, 1]);
    }

    #[test]
    fn missing_ranges_collapses_adjacent_runs() {
        let ranges = missing_ranges([true, true, true, true], 10);
        assert_eq!(ranges.as_wire(), &[10, 0, 4], "连续缺失应折成一段");
    }

    #[test]
    fn missing_ranges_is_empty_only_when_nothing_is_missing() {
        // 一块都不缺 → 空列表。而空列表在线路上表示"整个文件"，所以发送方会重传
        // 整个文件。这是 Go 的既有行为，不是笔误。
        assert!(missing_ranges([false, false], 100).is_whole());
        assert!(missing_ranges([], 100).is_whole());
        // 整个文件都缺 → 一个覆盖全文件的显式区间，不是空列表。
        assert_eq!(missing_ranges([true, true], 100).as_wire(), &[100, 0, 2]);
    }

    #[test]
    fn a_whole_file_counts_every_chunk() {
        let ranges = ChunkRanges::whole();
        assert!(ranges.is_whole());
        assert_eq!(ranges.count(1000), 1);
        assert_eq!(ranges.count(32769), 2);
        assert_eq!(ranges.bytes(32769), 32769);
        assert_eq!(ranges.count(0), 0);
        assert_eq!(ranges.bytes(0), 0);
        assert!(ranges.contains(999_999), "空列表请求所有块");
    }

    #[test]
    fn count_and_bytes_agree_with_the_go_helpers() {
        // 三段：[0, 200) 与 [400, 600) 与 [900, 1000)，块大小 100，文件 1000 字节。
        let ranges = ChunkRanges::from_wire(vec![100, 0, 2, 400, 2, 900, 1]);
        assert_eq!(ranges.count(1000), 5);
        assert_eq!(ranges.bytes(1000), 500);
        // 最后一个块被文件末尾裁掉：900 起、块大小 100 已经超出 1000。
        let truncated = ChunkRanges::from_wire(vec![100, 950, 3]);
        assert_eq!(truncated.count(1000), 3, "count 数块，不看文件大小");
        assert_eq!(truncated.bytes(1000), 50, "bytes 要裁到文件末尾");
    }

    #[test]
    fn count_ignores_non_positive_entries() {
        let ranges = ChunkRanges::from_wire(vec![100, 0, 0, 200, -3, 400, 2]);
        assert_eq!(ranges.count(1000), 2);
        assert_eq!(ranges.bytes(1000), 200);
    }

    #[test]
    fn contains_binary_searches_the_runs() {
        let ranges = ChunkRanges::from_wire(vec![100, 0, 1, 400, 2]);
        assert!(ranges.contains(0));
        assert!(ranges.contains(99));
        assert!(!ranges.contains(100), "第 0 段只覆盖 [0, 100)");
        assert!(!ranges.contains(399));
        assert!(ranges.contains(400));
        assert!(ranges.contains(599));
        assert!(!ranges.contains(600));
        // 比所有 start 都小 → 一段都不命中。
        let late = ChunkRanges::from_wire(vec![100, 500, 1]);
        assert!(!late.contains(0));
        assert!(late.contains(500));
    }

    #[test]
    fn contains_rejects_a_non_positive_chunk_size() {
        assert!(!ChunkRanges::from_wire(vec![0, 0, 1]).contains(0));
        assert!(!ChunkRanges::from_wire(vec![-1, 0, 1]).contains(0));
    }

    #[test]
    fn an_even_length_wire_list_is_tolerated() {
        // Go 的 `ChunkRangesToChunks` 在这里会越界 panic。对方送来的长度不由我们
        // 控制，所以 Rust 侧丢弃尾部那半对。
        let ranges = ChunkRanges::from_wire(vec![100, 0, 2, 400]);
        assert_eq!(ranges.count(1000), 2);
        assert_eq!(ranges.bytes(1000), 200);
        assert_eq!(ranges.chunk_offsets().collect::<Vec<_>>(), vec![0, 100]);
        assert!(!ranges.contains(400), "尾部那半对不参与判定");
    }

    #[test]
    fn chunk_offsets_expands_every_run() {
        let ranges = ChunkRanges::from_wire(vec![100, 0, 2, 400, 2]);
        assert_eq!(ranges.chunk_offsets().collect::<Vec<_>>(), vec![0, 100, 400, 500]);
        assert_eq!(ChunkRanges::whole().chunk_offsets().count(), 0, "空列表给不出偏移，先 materialize");
    }

    #[test]
    fn materialize_turns_whole_into_explicit_runs() {
        let ranges = ChunkRanges::whole().materialize(32769);
        assert_eq!(ranges.as_wire(), &[CHUNK_SIZE_I64, 0, 2]);
        assert_eq!(ranges.chunk_offsets().collect::<Vec<_>>(), vec![0, CHUNK_SIZE]);
        // 已经有显式区间时原样返回。
        let explicit = ChunkRanges::from_wire(vec![100, 0, 1]);
        assert_eq!(explicit.materialize(1000), explicit);
    }

    #[test]
    fn a_resume_request_round_trips_through_the_wire_form() {
        // 扫一个 5 块的文件：缺 1、2、4。
        let ranges = missing_ranges([false, true, true, false, true], 100);
        assert_eq!(ranges.as_wire(), &[100, 100, 2, 400, 1]);
        assert_eq!(ranges.count(500), 3);
        assert_eq!(ranges.bytes(500), 300);
        assert_eq!(ranges.chunk_offsets().collect::<Vec<_>>(), vec![100, 200, 400]);
        assert!(ranges.contains(100) && ranges.contains(299) && ranges.contains(400));
        assert!(!ranges.contains(0) && !ranges.contains(300));
        // 第 2 段从 400 起、一块 100 字节，所以覆盖到 499（文件的最后一个字节）。
        assert!(ranges.contains(499));
        assert!(!ranges.contains(500), "超出最后一段的末尾");
        // 往返一次线路形态不丢信息。
        assert_eq!(ChunkRanges::from_wire(ranges.clone().into_wire()), ranges);
    }

    #[test]
    fn is_all_zero_treats_an_empty_slice_as_zero() {
        assert!(is_all_zero(&[]));
        assert!(is_all_zero(&[0, 0, 0]));
        assert!(!is_all_zero(&[0, 1, 0]));
    }

    #[test]
    fn extreme_wire_values_do_not_overflow() {
        // 对方可以送任何 i64。这里只要求"不 panic、不卡住"，不要求结果有意义。
        // start = chunk_size = i64::MAX，所以第 0 个偏移刚好放得下（2 * i64::MAX 仍小于
        // u64::MAX），第 1 个就溢出了。
        let hostile = ChunkRanges::from_wire(vec![i64::MAX, i64::MAX, 3, i64::MIN, -1]);
        assert_eq!(hostile.count(u64::MAX), 3);
        assert_eq!(hostile.bytes(u64::MAX), 0, "乘出 u64 范围就整段丢掉");
        assert!(!hostile.contains(u64::MAX), "u64::MAX 塞不进 i64");
        assert_eq!(hostile.chunk_offsets().collect::<Vec<_>>(), vec![i64::MAX as u64, 2 * i64::MAX as u64]);
        let _ = missing_ranges([true; 4], u64::MAX);
    }
}
