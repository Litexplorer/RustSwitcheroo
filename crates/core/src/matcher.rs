//! 增量检索：4 个匹配器、单段文本汇总与窗口排名。
//!
//! 参数：用户输入 `query` 与窗口快照切片 `windows`
//! 核心流程：标题与进程名各跑一遍四个匹配器，两者得分之和作为排序键，按降序稳定排序；
//! 命中区间写进 [`MatchResult::ranges`]，供 US-042 做加粗高亮。输入含 `.` 时按
//! `进程.标题` 复合查询解析（US-041），不含 `.` 时保持单字段逻辑。只依赖 `std` 与
//! `pinyin` crate，不碰 UI 与 Windows API；拼音首字母（US-045）作为逐字匹配器的回退，
//! 见 [`crate::pinyin`]。

// 本模块的命中区间天然可能只有一段（前缀、子串都是单段），clippy 的
// `single_range_in_vec_init` 针对的是把 `[a; n]` 误写成 `vec![a..b]` 的笔误，
// 这里的 `vec![0..end]` 就是真实取值，故整模块关闭该检查。
#![allow(clippy::single_range_in_vec_init)]

use std::mem;
use std::ops::Range;

use crate::model::WindowInfo;
use crate::pinyin::{initials_with_all_readings, initials_with_map, original_range};

/// 内置进程别名：键入的单字符 → 完整进程名。
///
/// 参数：`(别名, 进程名)` 对照表
/// 核心流程：对应原版 `WindowFilterer.Filter` 里的 `switch (processFilterText)`，
/// 让 `p.wj` 这类查询少敲几个字母。
const PROCESS_ALIASES: [(char, &str); 4] = [
    ('e', "excel"),
    ('f', "explorer"),
    ('w', "word"),
    ('p', "powerpnt"),
];

/// 前缀命中得分：`text` 以 `query` 开头。
const SCORE_PREFIX: u32 = 4;
/// 单词首字母命中得分：`query` 依次匹配 `text` 的单词首字符。
const SCORE_SIGNIFICANT: u32 = 2;
/// 子串包含命中得分：`query` 连续出现在 `text` 中。
const SCORE_CONTAINS: u32 = 2;
/// 逐字顺序命中得分：`query` 的字符按顺序散落在 `text` 中。
const SCORE_INDIVIDUAL: u32 = 1;

/// 单段文本（标题或进程名）的匹配结果。
///
/// 参数：`matched` 是否命中；`score` 四个匹配器的得分之和；`ranges` 最高分匹配器的
/// 命中字符字节区间（升序、不重叠、已合并相邻段）
/// 核心流程：由 [`match_text`] 产出，[`rank_windows`] 把标题与进程名的结果打包进
/// [`RankedWindow`]，UI 只读 `ranges` 决定加粗哪些字符。
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct MatchResult {
    pub matched: bool,
    pub score: u32,
    /// 命中字符在原文中的字节区间，升序且不重叠，供 US-042 加粗高亮使用
    pub ranges: Vec<Range<usize>>,
}

/// 一个窗口的完整排名信息。
///
/// 参数：`index` 在传入切片中的原始下标；`score` 标题分 + 进程名分；
/// `title` / `process` 两段文本各自的匹配结果
/// 核心流程：由 [`rank_windows`] 产出并降序排序，UI 按 `index` 取回原窗口做渲染与切换。
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct RankedWindow {
    pub index: usize, // 在传入切片中的原始下标
    pub score: u32,   // 标题分 + 进程名分
    pub title: MatchResult,
    pub process: MatchResult,
}

/// 对单段文本跑 4 个匹配器并汇总。
///
/// 参数：`query` 用户输入；`text` 标题或进程名
/// 核心流程：依次跑前缀、单词首字母、子串包含、逐字四个匹配器，`score` 取四者之和，
/// `matched` 为任意一个命中，`ranges` 取最高分匹配器的区间（同分取顺序靠前者）并合并相邻段。
/// 空查询直接返回未命中，全量展示交给 [`rank_windows`] 处理。
pub fn match_text(query: &str, text: &str) -> MatchResult {
    if query.is_empty() {
        return MatchResult::default();
    }

    let candidates = [
        prefix_match(query, text),
        significant_characters_match(query, text),
        contains_match(query, text),
        individual_characters_match(query, text),
    ];

    let mut score = 0;
    let mut best_score = 0;
    let mut best_ranges: Vec<Range<usize>> = Vec::new();
    for mut candidate in candidates {
        score += candidate.score;
        // 严格大于，保证同分时保留匹配器顺序靠前的那个。
        if candidate.score > best_score {
            best_score = candidate.score;
            best_ranges = mem::take(&mut candidate.ranges);
        }
    }

    MatchResult {
        matched: best_score > 0,
        score,
        ranges: merge_adjacent(best_ranges),
    }
}

/// 按用户输入过滤窗口并排名（不带前台进程上下文）。
///
/// 参数：`query` 用户输入；`windows` 当前窗口快照切片（顺序即枚举顺序）
/// 核心流程：等价于 `foreground_process` 为 `None` 的 [`rank_windows_with_foreground`]；
/// 保留这个签名让既有调用方无需改动，行为与 US-041 之前完全一致。
pub fn rank_windows(query: &str, windows: &[WindowInfo]) -> Vec<RankedWindow> {
    rank_windows_with_foreground(query, windows, None)
}

/// 按用户输入过滤窗口并排名，支持 `进程.标题` 复合查询。
///
/// 参数：`query` 用户输入；`windows` 当前窗口快照切片（顺序即枚举顺序）；
/// `foreground_process` 唤起瞬间前台窗口的进程名（可为 `None`）
/// 核心流程：空查询原样返回全部窗口（分数 0、`matched=true`、`ranges` 为空）；不含 `.` 时
/// 退化成单字段逻辑，标题命中或进程名命中即保留；含 `.` 时只在**第一个** `.` 处切成
/// 「进程片段.标题片段」，两边都必须命中才保留，分数仍为标题分 + 进程名分，
/// 按降序稳定排序（同分保持枚举顺序）。对应原版 `WindowFilterer.Filter`。
pub fn rank_windows_with_foreground(
    query: &str,
    windows: &[WindowInfo],
    foreground_process: Option<&str>,
) -> Vec<RankedWindow> {
    if query.is_empty() {
        return windows_without_score(windows);
    }

    // 只有第一个 `.` 起分隔作用，标题片段里再出现的 `.` 属于标题本身。
    let Some((process_fragment, title_fragment)) = query.split_once('.') else {
        return rank_single_field(query, windows);
    };

    let process_filter = resolve_process_filter(process_fragment, foreground_process);
    let title_filter = non_empty(title_fragment);

    let mut ranked: Vec<RankedWindow> = windows
        .iter()
        .enumerate()
        .filter_map(|(index, window)| {
            // 片段为空（`p.`、或 `.报告` 且没有前台进程）时该字段不做限制。
            let title = match title_filter {
                Some(filter) => match_text(filter, &window.title),
                None => no_score_match(),
            };
            let process = match process_filter {
                Some(filter) => match_text(filter, &window.process_name),
                None => no_score_match(),
            };
            // 复合模式必须两边同时命中（原版 `ResultsTitle.Any(...) && ResultsProcessTitle.Any(...)`）。
            if !title.matched || !process.matched {
                return None;
            }
            Some(RankedWindow {
                index,
                score: title.score + process.score,
                title,
                process,
            })
        })
        .collect();

    // `sort_by` 是稳定排序，同分项保持枚举顺序。
    ranked.sort_by(|a, b| b.score.cmp(&a.score));
    ranked
}

/// 参数：`windows` 窗口快照切片
/// 核心流程：给每个窗口配一份「视为命中但 0 分、无高亮区间」的占位结果，保持枚举顺序。
/// 空查询与 `.` 这类两边都不限制的复合查询都走这里，返回全部窗口。
fn windows_without_score(windows: &[WindowInfo]) -> Vec<RankedWindow> {
    windows
        .iter()
        .enumerate()
        .map(|(index, _)| RankedWindow {
            index,
            score: 0,
            title: no_score_match(),
            process: no_score_match(),
        })
        .collect()
}

/// 参数：`query` 用户输入（非空且不含 `.`）；`windows` 窗口快照切片
/// 核心流程：US-041 之前的单字段逻辑——标题或进程名任一命中即保留，排序键为两者得分之和，
/// 降序稳定排序（对应原版 `processFilterText == null` 时的 `||` 分支）。
fn rank_single_field(query: &str, windows: &[WindowInfo]) -> Vec<RankedWindow> {
    let mut ranked: Vec<RankedWindow> = windows
        .iter()
        .enumerate()
        .filter_map(|(index, window)| {
            let title = match_text(query, &window.title);
            let process = match_text(query, &window.process_name);
            if !title.matched && !process.matched {
                return None;
            }
            Some(RankedWindow {
                index,
                score: title.score + process.score,
                title,
                process,
            })
        })
        .collect();

    ranked.sort_by(|a, b| b.score.cmp(&a.score));
    ranked
}

/// 参数：`fragment` 复合查询 `.` 左边的进程片段；`foreground_process` 唤起瞬间的前台进程名
/// 核心流程：去空白后非空就用它（整体等于单字符别名时先展开成完整进程名）；为空则回退到
/// 前台进程名（先去掉 `.exe` 后缀），回退后仍为空就返回 `None` 表示不做进程限制。
fn resolve_process_filter<'a>(
    fragment: &'a str,
    foreground_process: Option<&'a str>,
) -> Option<&'a str> {
    let fragment = fragment.trim();
    if !fragment.is_empty() {
        return Some(expand_process_alias(fragment));
    }

    let foreground = strip_exe_suffix(foreground_process?);
    if foreground.is_empty() {
        None
    } else {
        Some(foreground)
    }
}

/// 参数：`fragment` 已去空白的进程片段
/// 核心流程：整体恰好是一个字符且命中别名表（忽略大小写）时换成完整进程名，否则原样返回。
/// 对应原版的 `switch`：只有 `e`/`f`/`w`/`p` 四个简写，且必须整段相等才算。
fn expand_process_alias(fragment: &str) -> &str {
    let mut chars = fragment.chars();
    let (Some(only), None) = (chars.next(), chars.next()) else {
        return fragment;
    };
    match PROCESS_ALIASES
        .iter()
        .find(|(alias, _)| only.eq_ignore_ascii_case(alias))
    {
        Some((_, full)) => full,
        None => fragment,
    }
}

/// 参数：`name` 前台进程名
/// 核心流程：去掉首尾空白后，若以 `.exe`（忽略大小写）结尾就剥掉该后缀，
/// 让 `chrome.exe` 与窗口快照里的 `chrome` 能对上。
fn strip_exe_suffix(name: &str) -> &str {
    const SUFFIX: &[u8] = b".exe";
    let name = name.trim();
    match name.len().checked_sub(SUFFIX.len()) {
        // 后缀是纯 ASCII，切点必定落在字符边界上。
        Some(cut) if name.as_bytes()[cut..].eq_ignore_ascii_case(SUFFIX) => &name[..cut],
        _ => name,
    }
}

/// 参数：`fragment` 复合查询的标题片段
/// 核心流程：去空白后非空返回 `Some`，为空返回 `None` 表示标题不做限制（`p.` 只过滤进程）。
fn non_empty(fragment: &str) -> Option<&str> {
    let fragment = fragment.trim();
    if fragment.is_empty() {
        None
    } else {
        Some(fragment)
    }
}

/// 参数：无
/// 核心流程：构造「空查询下的占位结果」——视为命中但得分为 0、没有高亮区间，
/// 让空查询时 UI 与有查询时走同一套渲染路径。
fn no_score_match() -> MatchResult {
    MatchResult {
        matched: true,
        score: 0,
        ranges: Vec::new(),
    }
}

/// 参数：`query` 用户输入（非空）；`text` 待匹配文本
/// 核心流程：逐字符忽略大小写比较前缀，全部对上且 `query` 不越过 `text` 长度即命中，
/// 返回前缀的字节区间（对应原版 `StartsWithMatcher`，得分 4）。
fn prefix_match(query: &str, text: &str) -> MatchResult {
    // 空查询没有可比较的字符，直接判为未命中（全量展示由 rank_windows 负责）。
    if query.is_empty() {
        return MatchResult::default();
    }

    let mut wanted = query.chars();
    let mut end = 0;
    for (index, ch) in text.char_indices() {
        let Some(want) = wanted.next() else {
            break;
        };
        if !eq_ignore_case(want, ch) {
            return MatchResult::default();
        }
        end = index + ch.len_utf8();
    }

    // 循环结束时仍有剩余字符，说明 query 比 text 长，前缀不成立。
    if wanted.next().is_some() {
        return MatchResult::default();
    }

    MatchResult {
        matched: true,
        score: SCORE_PREFIX,
        ranges: vec![0..end],
    }
}

/// 参数：`query` 用户输入（非空）；`text` 待匹配文本
/// 核心流程：把 `text` 按「非字母数字」边界切成单词，`query` 的字符依次匹配单词首字符
/// （允许跳过前面的单词）；全部对上才算命中，返回各命中首字符的区间
/// （对应原版 `SignificantCharactersMatcher`，得分 2）。
fn significant_characters_match(query: &str, text: &str) -> MatchResult {
    let mut wanted = query.chars();
    let Some(mut want) = wanted.next() else {
        return MatchResult::default();
    };

    let mut ranges = Vec::new();
    let mut previous_is_word = false;
    for (index, ch) in text.char_indices() {
        let is_word = ch.is_alphanumeric();
        // 单词首字符：本身是字母数字，且前一个字符不是（或位于开头）。
        if is_word && !previous_is_word && eq_ignore_case(want, ch) {
            ranges.push(index..index + ch.len_utf8());
            match wanted.next() {
                Some(next) => want = next,
                None => {
                    return MatchResult {
                        matched: true,
                        score: SCORE_SIGNIFICANT,
                        ranges,
                    }
                }
            }
        }
        previous_is_word = is_word;
    }

    MatchResult::default()
}

/// 参数：`query` 用户输入（非空）；`text` 待匹配文本
/// 核心流程：忽略大小写查找 `query` 作为连续子串首次出现的位置，命中返回该子串区间
/// （对应原版 `ContainsMatcher`，得分 2）。原版用贪婪正则，重复出现时取最后一次；
/// 这里取第一次，高亮时更符合「为什么命中」的直觉，且原版用例中两者结果相同。
fn contains_match(query: &str, text: &str) -> MatchResult {
    let Some(first) = query.chars().next() else {
        return MatchResult::default();
    };

    for (start, ch) in text.char_indices() {
        if !eq_ignore_case(first, ch) {
            continue;
        }

        let mut end = start;
        let mut matched = true;
        let mut remaining = text[start..].char_indices();
        for want in query.chars() {
            match remaining.next() {
                Some((offset, candidate)) if eq_ignore_case(want, candidate) => {
                    end = start + offset + candidate.len_utf8();
                }
                _ => {
                    matched = false;
                    break;
                }
            }
        }

        if matched {
            return MatchResult {
                matched: true,
                score: SCORE_CONTAINS,
                ranges: vec![start..end],
            };
        }
    }

    MatchResult::default()
}

/// 参数：`query` 用户输入（非空）；`text` 待匹配文本
/// 核心流程：`query` 的字符按顺序在 `text` 中出现即可（可以不连续），每个命中字符各占一段区间
/// （对应原版 `IndividualCharactersMatcher`，得分 1）。优先在**原文**上逐字匹配，原文不命中时
/// 才回退到拼音首字母（US-045），保证英文文本的分数与区间与改动前完全一致，
/// 同一段文本也不会同时出现原文与拼音两套高亮。
fn individual_characters_match(query: &str, text: &str) -> MatchResult {
    let ranges = match ordered_char_ranges(query, text) {
        Some(ranges) => Some(ranges),
        None => pinyin_ranges(query, text),
    };

    match ranges {
        Some(ranges) => MatchResult {
            matched: true,
            score: SCORE_INDIVIDUAL,
            ranges,
        },
        None => MatchResult::default(),
    }
}

/// 参数：`query` 用户输入（非空）；`haystack` 待扫描的字符串（原文或其拼音首字母串）
/// 核心流程：`query` 的字符按顺序在 `haystack` 中逐个找，命中一个就记下它的字节区间并前进到
/// `query` 的下一个字符；`query` 耗尽即命中，扫完仍有剩余字符就是未命中（返回 `None`）。
/// 区间天然升序、不重叠——每个字符只被消费一次，且相邻两次命中之间是不同字符。
fn ordered_char_ranges(query: &str, haystack: &str) -> Option<Vec<Range<usize>>> {
    let mut wanted = query.chars();
    let mut want = wanted.next()?;

    let mut ranges = Vec::new();
    for (index, ch) in haystack.char_indices() {
        if eq_ignore_case(want, ch) {
            ranges.push(index..index + ch.len_utf8());
            match wanted.next() {
                Some(next) => want = next,
                None => return Some(ranges),
            }
        }
    }

    None
}

/// 参数：`query` 用户输入（非空）；`text` 待匹配文本
/// 核心流程：拼音回退，两级——先按首读音的首字母串匹配（`wj` → 「文件」），落空后再放开到
/// 多音字的全部读音（`yy` → 「音乐」，`乐` 另读 yuè）。每级都把首字母串上的命中用映射表
/// 换算回原文的字符区间；同一汉字贡献多个首字母时只保留一段，维持「升序且不重叠」。
fn pinyin_ranges(query: &str, text: &str) -> Option<Vec<Range<usize>>> {
    // 纯 ASCII 文本的拼音投影只是「转小写」，逐字匹配已经用 eq_ignore_case 覆盖过同样的
    // 比较，此处必然还是落空，直接跳过可以省掉一次投影与映射表分配。
    if text.is_ascii() {
        return None;
    }

    let (initials, map) = initials_with_map(text);
    if let Some(ranges) = initials_ranges(query, &initials, text, &map) {
        return Some(ranges);
    }

    let (initials, map) = initials_with_all_readings(text);
    initials_ranges(query, &initials, text, &map)
}

/// 参数：`query` 用户输入（非空）；`initials` 首字母串；`text` 首字母串对应的原文；
/// `map` 首字母串 → 原文的映射表
/// 核心流程：在首字母串上做与原文一致的逐字匹配，再把每个命中的首字母起点换算回原文区间；
/// 换算失败（映射表与首字母串不一致）按整体未命中处理，绝不产出半个字符的区间。
fn initials_ranges(
    query: &str,
    initials: &str,
    text: &str,
    map: &[(usize, usize)],
) -> Option<Vec<Range<usize>>> {
    let hits = ordered_char_ranges(query, initials)?;

    let mut ranges: Vec<Range<usize>> = Vec::with_capacity(hits.len());
    for hit in hits {
        let range = original_range(text, map, hit.start)?;
        // 多音字的多个首字母共享一段原文区间，同一汉字只登记一次。
        if ranges.last() != Some(&range) {
            ranges.push(range);
        }
    }

    (!ranges.is_empty()).then_some(ranges)
}

/// 参数：`a`、`b` 两个字符
/// 核心流程：先比原值，再用 `to_lowercase` 逐字符比较，覆盖 ASCII 与 Unicode 的大小写折叠。
fn eq_ignore_case(a: char, b: char) -> bool {
    a == b || a.to_lowercase().eq(b.to_lowercase())
}

/// 参数：`ranges` 升序、互不重叠的区间列表
/// 核心流程：首尾相接（`last.end == next.start`）的区间合成一段，其余原样保留；
/// 逐字匹配连续命中时（如 `abc` 命中 `ab c`）不会留下 `a`、`b` 两段碎片。
fn merge_adjacent(ranges: Vec<Range<usize>>) -> Vec<Range<usize>> {
    let mut merged: Vec<Range<usize>> = Vec::with_capacity(ranges.len());
    for range in ranges {
        match merged.last_mut() {
            Some(last) if last.end == range.start => last.end = range.end,
            _ => merged.push(range),
        }
    }
    merged
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::model::WindowId;

    /// 断言命中的结果与得分、区间完全一致。
    fn assert_matched(result: &MatchResult, score: u32, ranges: &[Range<usize>]) {
        assert!(result.matched, "应命中: {result:?}");
        assert_eq!(result.score, score, "{result:?}");
        assert_eq!(result.ranges, ranges, "{result:?}");
    }

    /// 断言未命中的结果得分与区间都是空的。
    fn assert_unmatched(result: &MatchResult) {
        assert!(!result.matched, "不应命中: {result:?}");
        assert_eq!(result.score, 0, "{result:?}");
        assert!(result.ranges.is_empty(), "{result:?}");
    }

    /// 区间必须升序、不重叠、不越界，且两端都落在字符边界上。
    fn assert_ranges_are_valid(text: &str, ranges: &[Range<usize>]) {
        let mut previous_end = 0;
        for range in ranges {
            assert!(range.start < range.end, "空区间或颠倒: {range:?}");
            assert!(range.end <= text.len(), "越界: {range:?} / {text:?}");
            assert!(range.start >= previous_end, "重叠或乱序: {ranges:?}");
            assert!(
                text.is_char_boundary(range.start),
                "起点不在字符边界: {range:?}"
            );
            assert!(
                text.is_char_boundary(range.end),
                "终点不在字符边界: {range:?}"
            );
            previous_end = range.end;
        }
    }

    fn window(title: &str, process_name: &str) -> WindowInfo {
        WindowInfo::new(WindowId::new(1), title, process_name)
    }

    /// 取出排名结果里的原始下标，便于断言「谁被保留、顺序如何」。
    fn indices(ranked: &[RankedWindow]) -> Vec<usize> {
        ranked.iter().map(|item| item.index).collect()
    }

    // ---------- StartsWithMatcher（原版 StartsWithMatcherTests.cs，StringParts 映射为 ranges） ----------

    #[test]
    fn starts_with_matcher_marks_prefix() {
        // 对应 Evaluate_InputStartsWithPattern_ResultIsMatched
        let result = prefix_match("google", "google chrome");
        assert!(result.matched);
    }

    #[test]
    fn starts_with_matcher_scores_four() {
        // 对应 Evaluate_InputStartsWithPattern_ScoreIsFour
        assert_eq!(prefix_match("google", "google chrome").score, 4);
    }

    #[test]
    fn starts_with_matcher_reports_prefix_range() {
        // 对应 Evaluate_InputStartsWithPattern_FirstStringPartIsMatch
        assert_eq!(prefix_match("google", "google chrome").ranges, vec![0..6]);
    }

    #[test]
    fn starts_with_matcher_leaves_rest_unmarked() {
        // 对应 Evaluate_InputStartsWithPattern_SecondStringPartIsNotMatch
        let text = "google chrome";
        let result = prefix_match("google", text);
        assert_eq!(&text[6..], " chrome");
        assert_eq!(result.ranges, vec![0..6]);
    }

    #[test]
    fn starts_with_matcher_rejects_empty_input() {
        // 对应 Evaluate_NullInput_ReturnsNotMatchingResult（Rust 侧以空串代替 null）
        assert_unmatched(&prefix_match("google", ""));
    }

    #[test]
    fn starts_with_matcher_rejects_empty_query() {
        // 对应 Evaluate_NullPattern_ReturnsNotMatchingResult
        assert_unmatched(&prefix_match("", "google chrome"));
    }

    #[test]
    fn starts_with_matcher_empty_query_has_no_ranges() {
        // 对应 Evaluate_NullPattern_ReturnsOneNonMatchingStringPart
        assert!(prefix_match("", "google chrome").ranges.is_empty());
    }

    #[test]
    fn starts_with_matcher_rejects_pattern_inside_text() {
        // 对应 Evaluate_InputContainsPattern_ReturnsNotMatchingResult
        assert_unmatched(&prefix_match("chrome", "google chrome"));
    }

    #[test]
    fn starts_with_matcher_inside_pattern_has_no_ranges() {
        // 对应 Evaluate_InputContainsPattern_ReturnsOneNonMatchingStringPart
        assert!(prefix_match("chrome", "google chrome").ranges.is_empty());
    }

    #[test]
    fn starts_with_matcher_ignores_casing_and_keeps_original_text() {
        // 对应 Evaluate_InputStartsWithPattern_CasingIsNotChanged
        let text = "Google Chrome";
        let result = prefix_match("google", text);
        assert_matched(&result, 4, &[0..6]);
        assert_eq!(&text[0..6], "Google");
        assert_eq!(&text[6..], " Chrome");
    }

    // ---------- ContainsMatcher（原版 ContainsMatcherTest.cs） ----------

    #[test]
    fn contains_matcher_rejects_empty_input() {
        // 对应 Evaluate_InputNull_ReturnsNonMatchingResult
        assert_unmatched(&contains_match("google", ""));
    }

    #[test]
    fn contains_matcher_empty_input_has_no_ranges() {
        // 对应 Evaluate_InputNull_ReturnsNonStringParts
        assert!(contains_match("google", "").ranges.is_empty());
    }

    #[test]
    fn contains_matcher_rejects_empty_query() {
        // 对应 Evaluate_PatternNull_ReturnsNonMatchingResult
        assert_unmatched(&contains_match("", "google chrome"));
    }

    #[test]
    fn contains_matcher_missing_pattern_scores_zero() {
        // 对应 Evaluate_InputDoesNotContainPattern_ScoreIsZero
        assert_eq!(contains_match("chrome", "google").score, 0);
    }

    #[test]
    fn contains_matcher_missing_pattern_has_no_ranges() {
        // 对应 Evaluate_InputDoesNotContainPattern_OneStringPart
        assert!(contains_match("chrome", "google").ranges.is_empty());
    }

    #[test]
    fn contains_matcher_hit_scores_two() {
        // 对应 Evaluate_InputContainsPattern_ScoreIsTwo
        assert_eq!(contains_match("chrome", "google chrome").score, 2);
    }

    #[test]
    fn contains_matcher_hit_is_matched() {
        // 对应 Evaluate_InputContainsPattern_ResultMatches
        assert!(contains_match("chrome", "google chrome").matched);
    }

    #[test]
    fn contains_matcher_hit_reports_single_range() {
        // 对应 Evaluate_InputContainsPattern_TwoStringParts（前后两段不计入命中区间）
        assert_eq!(
            contains_match("chrome", "google chrome").ranges,
            vec![7..13]
        );
    }

    #[test]
    fn contains_matcher_reports_matched_substring() {
        // 对应 Evaluate_InputContainsPattern_StringPartsAreCorrect
        let text = "google chrome";
        let result = contains_match("chrome", text);
        let range = result.ranges[0].clone();
        assert_eq!(&text[range], "chrome");
    }

    #[test]
    fn contains_matcher_pattern_at_start() {
        // 对应 Evaluate_InputContainsPatternInBeginning_StringPartsAreCorrect
        let text = "google chrome";
        let result = contains_match("google", text);
        assert_eq!(result.ranges, vec![0..6]);
        assert_eq!(&text[6..], " chrome");
    }

    #[test]
    fn contains_matcher_pattern_in_middle() {
        // 对应 Evaluate_InputContainsPatternInMiddle_StringPartsAreCorrect
        let text = "google chrome v28";
        let result = contains_match("chrome", text);
        assert_eq!(result.ranges, vec![7..13]);
        assert_eq!(&text[0..7], "google ");
        assert_eq!(&text[13..], " v28");
    }

    #[test]
    fn contains_matcher_ignores_casing() {
        // 对应 Evaluate_IgnoresCasing_ReturnsMatchingResult
        assert!(contains_match("chrome", "Google Chrome").matched);
    }

    // ---------- SignificantCharactersMatcher ----------
    // 原版 Core.UnitTests 没有该匹配器的用例（只有 StartsWith / Contains /
    // IndividualCharacters 三个文件），以下按原版正则的语义补测。

    #[test]
    fn significant_characters_match_word_initials() {
        let text = "google chrome";
        let result = significant_characters_match("gc", text);
        assert_matched(&result, 2, &[0..1, 7..8]);
        assert_eq!(&text[0..1], "g");
        assert_eq!(&text[7..8], "c");
    }

    #[test]
    fn significant_characters_skip_leading_words() {
        // 首字符允许跳过前面的单词，直接落在后面的单词首字符上。
        let text = "Mozilla Firefox";
        assert_eq!(significant_characters_match("f", text).ranges, vec![8..9]);
    }

    #[test]
    fn significant_characters_ignore_casing() {
        let text = "Google Chrome";
        assert_matched(&significant_characters_match("gc", text), 2, &[0..1, 7..8]);
    }

    #[test]
    fn significant_characters_require_every_query_char() {
        // 原版正则要求每一段都匹配，`cx` 在 `google chrome` 里找不到第二个单词首字母。
        assert_unmatched(&significant_characters_match("cx", "google chrome"));
    }

    #[test]
    fn significant_characters_reject_mid_word_characters() {
        // `h` 在 `chrome` 内部，不是单词首字符。
        assert_unmatched(&significant_characters_match("ch", "google chrome"));
    }

    #[test]
    fn significant_characters_handle_multibyte_words() {
        let text = "文件 - 报告";
        let result = significant_characters_match("文报", text);
        assert_matched(&result, 2, &[0..3, 9..12]);
        assert_ranges_are_valid(text, &result.ranges);
    }

    // ---------- IndividualCharactersMatcher（原版 IndividualCharactersMatcherTests.cs） ----------

    #[test]
    fn individual_characters_single_char_matched() {
        // 对应 Evaluate_InputContainsCharacter_ReturnsMatchingResult
        assert!(individual_characters_match("r", "chrome").matched);
    }

    #[test]
    fn individual_characters_single_char_scores_one() {
        // 对应 Evaluate_InputContainsCharacter_ReturnsCorrectScore
        assert_eq!(individual_characters_match("r", "chrome").score, 1);
    }

    #[test]
    fn individual_characters_single_char_reports_one_range() {
        // 对应 Evaluate_InputContainsCharacter_ReturnsThreeStringParts
        // （原版分片 ch | r | ome 三段，这里只记录命中段 r）
        let text = "chrome";
        let result = individual_characters_match("r", text);
        assert_matched(&result, 1, &[2..3]);
        assert_eq!(&text[0..2], "ch");
        assert_eq!(&text[2..3], "r");
        assert_eq!(&text[3..], "ome");
    }

    #[test]
    fn individual_characters_two_chars_spread_out() {
        // 对应 Evaluate_InputContainsTwoCharacters_ReturnsFourStringParts
        // （原版分片 ch | r | om | e 四段）
        let text = "chrome";
        let result = individual_characters_match("re", text);
        assert_matched(&result, 1, &[2..3, 5..6]);
        assert_eq!(&text[0..2], "ch");
        assert_eq!(&text[3..5], "om");
    }

    #[test]
    fn individual_characters_two_chars_spread_out_again() {
        // 对应 Evaluate_InputContainsTwoCharacters2_ReturnsFourStringParts
        // （原版分片 c | hrom | e 三段）
        let text = "chrome";
        let result = individual_characters_match("ce", text);
        assert_matched(&result, 1, &[0..1, 5..6]);
        assert_eq!(&text[1..5], "hrom");
    }

    #[test]
    fn individual_characters_reject_empty_input() {
        // 对应 Evaluate_InputNull_ReturnsNonMatchingResult
        assert_unmatched(&individual_characters_match("crm", ""));
    }

    #[test]
    fn individual_characters_empty_input_has_no_ranges() {
        // 对应 Evaluate_InputNull_ReturnsNoStringParts
        assert!(individual_characters_match("crm", "").ranges.is_empty());
    }

    #[test]
    fn individual_characters_reject_empty_query() {
        // 对应 Evaluate_PatternNull_ReturnsNonMatchingResult
        assert_unmatched(&individual_characters_match("", "chrome"));
    }

    #[test]
    fn individual_characters_empty_query_has_no_ranges() {
        // 对应 Evaluate_PatternNull_ReturnsOneNonMatchingStringPart
        assert!(individual_characters_match("", "chrome").ranges.is_empty());
    }

    #[test]
    fn individual_characters_prefer_literal_match_over_pinyin() {
        // 原文能逐字命中时绝不走拼音回退：区间落在字面的 w、j 上，而不是「文件」。
        let text = "文件wj";
        let result = individual_characters_match("wj", text);
        assert_matched(&result, 1, &[6..7, 7..8]);
        assert_eq!(&text[6..7], "w");
        assert_eq!(&text[7..8], "j");
    }

    #[test]
    fn individual_characters_fall_back_to_pinyin_initials() {
        // `wj` 在原文里凑不出来，回退到拼音首字母，区间换算回「文」「件」两个汉字。
        let text = "文件 - PowerPoint";
        let result = individual_characters_match("wj", text);
        assert_matched(&result, 1, &[0..3, 3..6]);
        assert_eq!(&text[0..3], "文");
        assert_eq!(&text[3..6], "件");
        assert_ranges_are_valid(text, &result.ranges);
    }

    #[test]
    fn individual_characters_use_polyphonic_readings_when_needed() {
        // 乐 的首读音是 lè（首读音首字母串为 yl），`yy` 要靠多音字 yuè 才命中。
        let text = "音乐播放器";
        let result = individual_characters_match("yy", text);
        assert_matched(&result, 1, &[0..3, 3..6]);
        assert_eq!(&text[0..6], "音乐");
        assert_ranges_are_valid(text, &result.ranges);
    }

    // ---------- 汇总与工具 ----------

    #[test]
    fn match_text_sums_all_matchers() {
        // google chrome + google：前缀 4 + 子串 2 + 逐字 1 = 7；单词首字母未命中
        // （query 有 6 个字符，文本只有 google、chrome 两个单词首字母，第二个字符就对不上）。
        let result = match_text("google", "google chrome");
        assert_matched(&result, 7, &[0..6]);
    }

    #[test]
    fn match_text_keeps_ranges_of_highest_scoring_matcher() {
        // chrome + chr：前缀 4 + 子串 2 + 逐字 1，区间取前缀的 0..3
        assert_matched(&match_text("chr", "chrome"), 7, &[0..3]);
    }

    #[test]
    fn match_text_merges_adjacent_ranges() {
        // ab c + abc：只有逐字匹配命中，a、b 两段相邻合成 0..2，c 单独一段。
        let text = "ab c";
        assert_eq!(
            individual_characters_match("abc", text).ranges,
            vec![0..1, 1..2, 3..4]
        );
        assert_matched(&match_text("abc", text), 1, &[0..2, 3..4]);
    }

    #[test]
    fn match_text_returns_zero_when_nothing_matches() {
        assert_unmatched(&match_text("zzz", "chrome"));
    }

    #[test]
    fn match_text_treats_empty_query_as_no_match() {
        // 空查询由 rank_windows 统一处理全量展示，单段文本不产生命中。
        assert_unmatched(&match_text("", "google chrome"));
    }

    #[test]
    fn match_text_hits_chinese_title_by_pinyin_initials() {
        // US-045：「文件 - PowerPoint」的原文凑不出 w、j，只有逐字匹配器经拼音回退命中 1 分；
        // 两个汉字相邻，汇总时合并成一段 0..6。
        let text = "文件 - PowerPoint";
        let result = match_text("wj", text);
        assert_matched(&result, 1, &[0..6]);
        assert_eq!(&text[result.ranges[0].clone()], "文件");
    }

    #[test]
    fn match_text_hits_polyphonic_title_by_pinyin_initials() {
        // 乐 还有 yuè 一读，所以 `yy` 能命中「音乐播放器」的两个首字。
        let text = "音乐播放器";
        let result = match_text("yy", text);
        assert!(result.matched, "应命中: {result:?}");
        assert_eq!(result.score, 1, "{result:?}");
        assert_eq!(&text[result.ranges[0].clone()], "音乐");
        assert_ranges_are_valid(text, &result.ranges);
    }

    #[test]
    fn match_text_keeps_ascii_results_unchanged_by_pinyin() {
        // 英文文本原文就能逐字命中，拼音回退不会掺进来：得分与区间与改动前完全一致。
        assert_matched(&match_text("pp", "PowerPoint"), 1, &[0..1, 5..6]);
    }

    #[test]
    fn match_text_empty_query_is_unaffected_by_pinyin() {
        // 拼音投影不能改变空查询行为：仍然不命中、没有区间。
        assert_unmatched(&match_text("", "文件"));
        assert_unmatched(&match_text("", "音乐播放器"));
    }

    #[test]
    fn merge_adjacent_ranges_combines_touching_ranges() {
        assert_eq!(merge_adjacent(vec![0..1, 1..2, 2..3]), vec![0..3]);
        assert_eq!(merge_adjacent(vec![0..1, 2..3]), vec![0..1, 2..3]);
        assert_eq!(merge_adjacent(Vec::new()), Vec::new());
    }

    #[test]
    fn ranges_never_break_character_boundaries() {
        let cases = [
            ("文报", "文件 - 报告"),
            ("本キ", "日本語テキスト"),
            ("chr", "chrome"),
            ("gc", "Google Chrome"),
            ("d", "Ünïcodé"),
        ];
        for (query, text) in cases {
            let result = match_text(query, text);
            assert_ranges_are_valid(text, &result.ranges);
        }
    }

    #[test]
    fn chinese_query_matches_character_by_character() {
        // 件、告都不是单词首字符，只有逐字匹配能命中。
        let text = "文件 - 报告";
        let result = match_text("件告", text);
        assert_matched(&result, 1, &[3..6, 12..15]);
        assert_eq!(&text[3..6], "件");
        assert_eq!(&text[12..15], "告");
    }

    // ---------- rank_windows ----------

    #[test]
    fn rank_windows_returns_everything_for_empty_query() {
        let windows = vec![window("alpha", "a.exe"), window("beta", "b.exe")];
        let ranked = rank_windows("", &windows);
        assert_eq!(ranked.len(), 2);
        assert_eq!(
            ranked.iter().map(|item| item.index).collect::<Vec<_>>(),
            vec![0, 1]
        );
        for item in &ranked {
            assert_eq!(item.score, 0);
            assert!(item.title.matched && item.process.matched);
            assert!(item.title.ranges.is_empty() && item.process.ranges.is_empty());
        }
    }

    #[test]
    fn rank_windows_keeps_process_only_match() {
        let windows = vec![window("无标题 - 记事本", "chrome")];
        let ranked = rank_windows("chrome", &windows);
        assert_eq!(ranked.len(), 1);
        assert_eq!(ranked[0].index, 0);
        assert!(!ranked[0].title.matched);
        assert!(ranked[0].process.matched);
        assert_eq!(ranked[0].score, ranked[0].process.score);
        assert!(ranked[0].score > 0);
    }

    #[test]
    fn rank_windows_drops_windows_without_any_match() {
        let windows = vec![window("chrome", "chrome"), window("zzz", "zzz")];
        let ranked = rank_windows("chrome", &windows);
        assert_eq!(ranked.len(), 1);
        assert_eq!(ranked[0].index, 0);
    }

    #[test]
    fn rank_windows_sorts_descending_and_keeps_ties_in_order() {
        // 三条命中：x alpha 与 x beta 同分（均为 9 分），必须保持枚举顺序；ffx 更低排最后。
        let windows = vec![
            window("ffx", "zzz"),
            window("x alpha", "zzz"),
            window("x beta", "zzz"),
        ];
        let ranked = rank_windows("x", &windows);
        assert_eq!(
            ranked.iter().map(|item| item.index).collect::<Vec<_>>(),
            vec![1, 2, 0]
        );
        assert!(ranked[0].score > ranked[2].score);
    }

    #[test]
    fn rank_windows_scores_title_and_process_together() {
        let windows = vec![window("chrome", "chrome")];
        let ranked = rank_windows("chrome", &windows);
        assert_eq!(
            ranked[0].score,
            ranked[0].title.score + ranked[0].process.score
        );
        assert!(ranked[0].title.matched && ranked[0].process.matched);
    }

    #[test]
    fn rank_windows_chinese_query_works() {
        let windows = vec![
            window("文件 - PowerPoint", "powerpnt"),
            window("终端", "WindowsTerminal"),
        ];
        let ranked = rank_windows("件p", &windows);
        assert_eq!(ranked.len(), 1);
        assert_eq!(ranked[0].index, 0);
        assert_ranges_are_valid(&windows[0].title, &ranked[0].title.ranges);
    }

    // ---------- 复合查询 `进程.标题`（US-041，对应原版 WindowFilterer.Filter） ----------

    #[test]
    fn composite_query_requires_process_and_title_both_to_match() {
        // 对应 Filter 里 `processFilterText != null` 分支：标题与进程必须同时命中（&&）。
        let windows = vec![
            window("月度报告 wj", "powerpnt"), // 两边都命中
            window("月度报告 wj", "code"),     // 只有标题命中
            window("年度总结", "powerpnt"),    // 只有进程命中
        ];

        let ranked = rank_windows_with_foreground("p.wj", &windows, None);

        assert_eq!(indices(&ranked), vec![0]);
        let hit = &ranked[0];
        assert!(hit.title.matched && hit.process.matched);
        assert_eq!(hit.score, hit.title.score + hit.process.score);
        assert_eq!(hit.score, 10); // 标题 3 分 + 进程 7 分
        assert_ranges_are_valid(&windows[0].title, &hit.title.ranges);
        assert_ranges_are_valid(&windows[0].process_name, &hit.process.ranges);
    }

    #[test]
    fn composite_query_excludes_title_only_and_process_only_rows() {
        // 分开断言两侧各自能把对方筛掉，避免上面的用例失败时分不清是哪条规则。
        let process_only = vec![window("年度总结", "powerpnt")];
        assert!(rank_windows_with_foreground("p.wj", &process_only, None).is_empty());

        let title_only = vec![window("月度报告 wj", "code")];
        assert!(rank_windows_with_foreground("p.wj", &title_only, None).is_empty());
    }

    #[test]
    fn composite_query_process_alias_e_equals_excel() {
        // 对应原版 `case "e": processFilterText = "excel"`。
        let windows = vec![window("数据表 x", "excel"), window("数据表 x", "notepad")];

        let short = rank_windows_with_foreground("e.x", &windows, None);
        let long = rank_windows_with_foreground("excel.x", &windows, None);

        assert_eq!(indices(&short), vec![0]);
        assert_eq!(short, long);
    }

    #[test]
    fn composite_query_process_alias_is_case_insensitive() {
        // 规则要求别名大小写不敏感（原版 switch 是大小写敏感，这里按 US-041 契约放宽）。
        let windows = vec![window("数据表 x", "excel"), window("数据表 x", "notepad")];

        assert_eq!(
            rank_windows_with_foreground("E.x", &windows, None),
            rank_windows_with_foreground("excel.x", &windows, None)
        );
    }

    #[test]
    fn composite_query_process_aliases_cover_all_four_letters() {
        // 四个简写各自展开：e→excel、f→explorer、w→word、p→powerpnt。
        let cases = [
            ("e.x", "excel"),
            ("f.x", "explorer"),
            ("w.x", "word"),
            ("p.x", "powerpnt"),
        ];
        for (query, process_name) in cases {
            let windows = vec![window("表格 x", process_name), window("表格 x", "code")];
            assert_eq!(
                indices(&rank_windows_with_foreground(query, &windows, None)),
                vec![0],
                "{query} 未展开成 {process_name}"
            );
        }
    }

    #[test]
    fn composite_query_alias_only_applies_to_a_lone_letter() {
        // `ex` 不是别名（必须整段等于单字符），它靠普通前缀匹配命中 excel。
        let windows = vec![window("表格 x", "excel"), window("表格 x", "code")];
        assert_eq!(
            indices(&rank_windows_with_foreground("ex.x", &windows, None)),
            vec![0]
        );
    }

    #[test]
    fn composite_query_empty_process_fragment_uses_foreground_process() {
        // 对应原版 `if (processFilterText.Length == 0) processFilterText = context.ForegroundWindowProcessTitle`。
        let windows = vec![window("月度报告", "word"), window("月度报告", "excel")];

        let ranked = rank_windows_with_foreground(".报告", &windows, Some("word.exe"));

        assert_eq!(indices(&ranked), vec![0]);
        assert_eq!(
            ranked[0].score,
            ranked[0].title.score + ranked[0].process.score
        );
    }

    #[test]
    fn composite_query_foreground_process_drops_exe_suffix() {
        // 前台进程名常带 .exe，去掉后缀后要能与窗口快照里的裸进程名对上。
        for foreground in ["word.exe", "WORD.EXE", " word "] {
            let windows = vec![window("月度报告", "word"), window("月度报告", "excel")];
            assert_eq!(
                indices(&rank_windows_with_foreground(
                    ".报告",
                    &windows,
                    Some(foreground)
                )),
                vec![0],
                "foreground={foreground}"
            );
        }
    }

    #[test]
    fn composite_query_without_foreground_process_only_filters_title() {
        // foreground_process 为 None 时不做进程限制，但标题仍必须命中。
        let windows = vec![
            window("月度报告", "word"),
            window("月度报告", "excel"),
            window("年度总结", "word"),
        ];

        let ranked = rank_windows_with_foreground(".报告", &windows, None);

        assert_eq!(indices(&ranked), vec![0, 1]);
        for item in &ranked {
            assert!(item.process.matched);
            assert_eq!(item.process.score, 0);
            assert!(item.process.ranges.is_empty());
            assert!(item.title.matched && item.title.score > 0);
        }
        // 不带前台进程的同名入口行为必须一致。
        assert_eq!(ranked, rank_windows(".报告", &windows));
    }

    #[test]
    fn composite_query_empty_title_fragment_only_filters_process() {
        // `p.`：标题不做限制，仍要求进程命中。
        let windows = vec![
            window("任意标题", "powerpnt"),
            window("wj", "code"),
            window("别的", "notepad"),
        ];

        let ranked = rank_windows_with_foreground("p.", &windows, None);

        assert_eq!(indices(&ranked), vec![0]);
        assert!(ranked[0].title.matched);
        assert!(ranked[0].title.ranges.is_empty());
        assert_eq!(ranked[0].score, ranked[0].process.score);
        assert!(ranked[0].score > 0);
    }

    #[test]
    fn composite_query_single_dot_equals_empty_query() {
        // `.` 两边都等于空：进程片段回退到 None、标题片段也为空 → 返回全部窗口。
        let windows = vec![
            window("alpha", "excel"),
            window("beta", "code"),
            window("gamma", "notepad"),
        ];

        let ranked = rank_windows_with_foreground(".", &windows, None);

        assert_eq!(indices(&ranked), vec![0, 1, 2]);
        assert!(ranked.iter().all(|item| item.score == 0));
        assert_eq!(ranked, rank_windows("", &windows));
    }

    #[test]
    fn composite_query_trims_both_fragments() {
        // 进程片段与标题片段各自 trim，多余的空白不参与匹配。
        let windows = vec![
            window("月度报告 wj", "powerpnt"),
            window("月度报告", "code"),
        ];
        let ranked = rank_windows_with_foreground("  p .  wj  ", &windows, None);
        assert_eq!(indices(&ranked), vec![0]);
    }

    #[test]
    fn composite_query_only_splits_at_the_first_dot() {
        // 只在第一个 `.` 处切分：`a.b` 整体是标题片段。
        let windows = vec![
            window("a.b 报告", "powerpnt"),
            window("a b 报告", "powerpnt"),
        ];
        assert_eq!(
            indices(&rank_windows_with_foreground("p.a.b", &windows, None)),
            vec![0]
        );
    }

    #[test]
    fn composite_query_ranges_point_at_each_field() {
        // 供 US-042 高亮：标题区间落在窗口标题里，进程区间落在进程名里。
        let windows = vec![window("月度报告 wj", "PowerPnt")];

        let ranked = rank_windows_with_foreground("p.wj", &windows, None);

        assert_eq!(ranked.len(), 1);
        let item = &ranked[0];
        assert_matched(&item.title, 3, &[13..15]);
        assert_matched(&item.process, 7, &[0..8]);
        assert_eq!(&windows[0].title[item.title.ranges[0].clone()], "wj");
        assert_eq!(
            &windows[0].process_name[item.process.ranges[0].clone()],
            "PowerPnt"
        );
        assert_eq!(item.score, 10);
    }

    #[test]
    fn composite_query_matches_title_by_pinyin_initials() {
        // US-045 的原版样例：进程 powerpnt + 标题含「文件」，`p.wj` 依然命中并高亮「文件」。
        let windows = vec![window("文件 - PowerPoint", "powerpnt")];

        let ranked = rank_windows_with_foreground("p.wj", &windows, None);

        assert_eq!(ranked.len(), 1);
        let item = &ranked[0];
        assert_matched(&item.title, 1, &[0..6]);
        assert_eq!(&windows[0].title[item.title.ranges[0].clone()], "文件");
        assert_matched(&item.process, 7, &[0..8]);
        assert_eq!(item.score, 8);
    }

    #[test]
    fn composite_query_sorts_descending_and_keeps_ties_in_order() {
        // 分数仍为标题分 + 进程分，降序稳定排序：同分的 0 与 2 保持枚举顺序。
        let windows = vec![
            window("alpha x", "excel"),
            window("x beta", "excel"),
            window("gamma x", "excel"),
        ];

        let ranked = rank_windows_with_foreground("e.x", &windows, None);

        assert_eq!(indices(&ranked), vec![1, 0, 2]);
        assert_eq!(
            ranked.iter().map(|item| item.score).collect::<Vec<_>>(),
            vec![16, 12, 12]
        );
    }

    #[test]
    fn composite_query_returns_everything_for_empty_query() {
        // query 本身为空串时仍返回全部窗口，与前台进程无关。
        let windows = vec![window("alpha", "excel"), window("beta", "code")];
        let ranked = rank_windows_with_foreground("", &windows, Some("word.exe"));
        assert_eq!(indices(&ranked), vec![0, 1]);
        assert!(ranked.iter().all(|item| item.score == 0));
    }

    #[test]
    fn non_composite_query_is_unaffected_by_foreground_process() {
        // 不含 `.` 时行为与改动前一致：标题或进程命中即保留，foreground_process 被忽略。
        let windows = vec![
            window("无标题 - 记事本", "chrome"),
            window("chrome", "chrome"),
            window("zzz", "zzz"),
        ];

        let baseline = rank_windows("chrome", &windows);
        assert_eq!(indices(&baseline), vec![1, 0]);
        assert_eq!(
            baseline,
            rank_windows_with_foreground("chrome", &windows, None)
        );
        assert_eq!(
            baseline,
            rank_windows_with_foreground("chrome", &windows, Some("excel.exe"))
        );
        // 不含 `.` 时不要求两边同时命中：只有进程命中的窗口仍在。
        let process_only = &baseline[1];
        assert!(!process_only.title.matched && process_only.process.matched);
    }

    /// 构造 `count` 个窗口：标题与进程名按固定表轮换，附加序号保证互不相同。
    fn many_windows(count: usize) -> Vec<WindowInfo> {
        const TITLES: [&str; 6] = [
            "Google Chrome - 新标签页",
            "文件 - PowerPoint",
            "Visual Studio Code - main.rs",
            "无标题 - 记事本",
            "Windows 资源管理器",
            "终端 - root@host:~",
        ];
        const PROCESSES: [&str; 6] = [
            "chrome",
            "powerpnt",
            "code",
            "notepad",
            "explorer",
            "WindowsTerminal",
        ];

        (0..count)
            .map(|i| {
                WindowInfo::new(
                    WindowId::new(i as u64 + 1),
                    format!("{} #{i}", TITLES[i % TITLES.len()]),
                    PROCESSES[i % PROCESSES.len()],
                )
            })
            .collect()
    }

    #[test]
    fn rank_windows_handles_1000_windows() {
        let windows = many_windows(1000);
        let ranked = rank_windows("chr", &windows);

        // 只有 chrome 窗口能凑齐 c、h、r：标题 "Chrome" 子串命中，进程名 chrome 前缀命中。
        let expected = windows
            .iter()
            .filter(|w| w.process_name == "chrome")
            .count();
        assert_eq!(ranked.len(), expected);
        assert!(ranked.iter().all(|item| item.score == 10));
        assert!(ranked.windows(2).all(|pair| pair[0].score >= pair[1].score));
        for item in &ranked {
            assert_ranges_are_valid(&windows[item.index].title, &item.title.ranges);
            assert_ranges_are_valid(&windows[item.index].process_name, &item.process.ranges);
        }
    }

    /// 基准：1000 个窗口的完整过滤加排序。
    /// 运行方式：`cd RustSwitcheroo && cargo test -p switcheroo-core --release -- --ignored`
    #[test]
    #[ignore = "基准测试，用 cargo test -p switcheroo-core --release -- --ignored 运行"]
    fn bench_filter_1000_windows_under_16ms() {
        let windows = many_windows(1000);
        let query = "chr";

        // 预热一次，避免把首次分配与指令缓存未命中算进基准。
        let _ = std::hint::black_box(rank_windows(query, &windows));

        let started = std::time::Instant::now();
        let ranked = std::hint::black_box(rank_windows(std::hint::black_box(query), &windows));
        let elapsed = started.elapsed();

        println!(
            "bench_filter_1000_windows: {} 条结果，耗时 {:.3} ms",
            ranked.len(),
            elapsed.as_secs_f64() * 1000.0
        );
        assert!(!ranked.is_empty());
        assert!(
            elapsed < std::time::Duration::from_millis(16),
            "1000 个窗口过滤耗时 {elapsed:?}，超过 16ms"
        );
    }
}
