//! 命中区间的 UTF-8 → UTF-16 换算（S6 加粗高亮用）。
//!
//! 参数：无（纯函数 + 单测）
//! 核心流程：`core` 的 [`switcheroo_core::MatchResult::ranges`] 给的是 UTF-8 字节区间，
//! 而 DirectWrite 的 `DWRITE_TEXT_RANGE` 用的是 UTF-16 code unit 偏移。本模块只做这一层
//! 换算：逐字符累加 `char::len_utf16()`，把「字节边界」翻译成「UTF-16 偏移」。
//! CJK 基本区一个字符 = 3 字节 / 1 个 UTF-16 unit；emoji 等 4 字节字符 = 1 个字符 /
//! 2 个 UTF-16 unit（代理对），所以两者不能混用。

use std::ops::Range;

/// 参数：`text` 原始文本（与 `ranges` 属于同一份字符串）；`ranges` UTF-8 字节区间，
/// 可能为空、可能越界、也可能落在多字节字符中间。
/// 核心流程：逐个区间做四步校验（非空 / 不越界 / 两端都落在字符边界上）→ 用
/// `char_indices` 一次遍历把「字节偏移」映射成「UTF-16 偏移」，每步累加 `len_utf16()`
/// （基本区字符 +1、代理对 +2）→ 换算后长度仍大于 0 才收下。越界、反向、非字符边界的
/// 区间一律忽略（宁可少加粗，也不能让 DirectWrite 收到非法 range）。
pub fn utf8_range_to_utf16_range(text: &str, ranges: &[Range<usize>]) -> Vec<Range<usize>> {
    let mut utf16_ranges = Vec::with_capacity(ranges.len());
    for range in ranges {
        if range.start >= range.end || range.end > text.len() {
            continue;
        }

        // 同一份 text 上可能有好几个区间，最省事的做法是每个区间各走一遍：
        // 区间数量是个位数、文本也就一行标题那么长，没必要预先建映射表。
        let mut start = None;
        let mut end = None;
        let mut offset = 0;
        for (byte, character) in text.char_indices() {
            if byte == range.start {
                start = Some(offset);
            }
            if byte == range.end {
                end = Some(offset);
            }
            offset += character.len_utf16();
        }
        // 区间末尾正好是字符串结尾时，`char_indices` 不会产出这个位置，单独补一次。
        if range.end == text.len() {
            end = Some(offset);
        }

        // start / end 都是 None 说明端点不在字符边界上（比如切在 3 字节汉字的中间）。
        if let (Some(start), Some(end)) = (start, end) {
            if end > start {
                utf16_ranges.push(start..end);
            }
        }
    }
    utf16_ranges
}

#[cfg(test)]
mod tests {
    // 单测里 `&[0..3]` / `3..1` 就是最直白的写法：前者是「只命中一个词的区间」，
    // 后者是「故意构造出来的反向区间」。这两条 clippy 规则针对的是业务代码里误写
    // 单元素 Vec / 空区间的情况，在这里是误报。
    #![allow(clippy::single_range_in_vec_init, clippy::reversed_empty_ranges)]

    use super::*;

    /// 纯 ASCII：一个字节就是一个 UTF-16 unit，区间原样返回。
    #[test]
    fn ascii_ranges_are_unchanged() {
        let text = "hello world";
        assert_eq!(
            utf8_range_to_utf16_range(text, &[0..2, 6..11]),
            vec![0..2, 6..11]
        );
    }

    /// 中文：每个汉字 3 字节但只占 1 个 UTF-16 unit，字节区间要被压缩 3 倍。
    #[test]
    fn cjk_ranges_shrink_by_three() {
        let text = "微信开发者工具";
        assert_eq!(utf8_range_to_utf16_range(text, &[0..3]), vec![0..1]);
        assert_eq!(utf8_range_to_utf16_range(text, &[3..9]), vec![1..3]);
        assert_eq!(utf8_range_to_utf16_range(text, &[0..9]), vec![0..3]);
    }

    /// emoji（4 字节 / 2 个 UTF-16 unit）：按代理对计算，不能按字节也不能按字符算。
    #[test]
    fn emoji_counts_as_two_utf16_units() {
        let text = "a😀b";
        // 字节：`a` = 0..1、`😀` = 1..5、`b` = 5..6；
        // UTF-16：`a` 占 0..1、`😀` 占 1..3（代理对）、`b` 占 3..4。
        assert_eq!(utf8_range_to_utf16_range(text, &[1..5]), vec![1..3]);
        assert_eq!(utf8_range_to_utf16_range(text, &[0..6]), vec![0..4]);
        assert_eq!(utf8_range_to_utf16_range(text, &[5..6]), vec![3..4]);
    }

    /// 越界、反向、切在字符中间的区间一律丢弃，绝不产生非法 UTF-16 区间。
    #[test]
    fn invalid_ranges_are_ignored() {
        let text = "中文ab";
        let utf16 = utf8_range_to_utf16_range(text, &[0..99, 3..1, 1..2, 3..3, 99..100]);
        assert!(utf16.is_empty(), "{utf16:?}");
    }
}
