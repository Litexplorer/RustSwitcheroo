//! 拼音首字母投影：把标题 / 进程名压成「首字母串」，并保留回原文字节的映射。
//!
//! 参数：任意 UTF-8 文本（窗口标题或进程名）
//! 核心流程：逐字符产出首字母——汉字取拼音首字母，非汉字原样保留（拉丁字母转小写）。
//! 映射表登记首字母串中每个字符的字节起点与原文对应字符的字节起点，[`crate::matcher`]
//! 据此把首字母串上的命中换算回原文区间做加粗高亮（US-045）。首读音与多音字的全部读音
//! 分成两个入口：默认只取首读音（`文件` → `wj`，最贴近用户直觉），逐字失败后
//! [`crate::matcher`] 才放开到全部读音（`音乐` 的 `乐` 另读 yuè，`yy` 才命中）。
//! 全程只用 `std` 与 `pinyin` crate，不碰 UI 与 Windows API，也没有 `unsafe`。

use std::ops::Range;

use pinyin::ToPinyinMulti;

/// 把原文投影成首字母串（每字只取首读音），并给出「首字母串 → 原文」的映射表。
///
/// 参数：`text` 待投影的原文；返回值第一项是首字母串，第二项是映射表，
/// 每个条目为 `(首字母串中的字节起点, 原文中的字节起点)`
/// 核心流程：见 [`project`]，`all_readings` 取 `false`——汉字只取第一个读音的首字母，
/// 因此首字母串的长度与原文的字符数一一对应（`文件` → `wj`）。
pub fn initials_with_map(text: &str) -> (String, Vec<(usize, usize)>) {
    project(text, false)
}

/// 同 [`initials_with_map`]，但汉字会贡献其全部读音的首字母（`乐` → `l` 与 `y`）。
///
/// 参数：`text` 待投影的原文；返回值同 [`initials_with_map`]
/// 核心流程：见 [`project`]，`all_readings` 取 `true`——多音字把每个读音的首字母都追加
/// 进首字母串（同一汉字内去重），这些条目共享同一段原文区间；只在逐字与首读音都落空时
/// 才作为第二级回退使用，避免低频读音把匹配放得太松。
pub fn initials_with_all_readings(text: &str) -> (String, Vec<(usize, usize)>) {
    project(text, true)
}

/// 把原文投影成首字母串并记录映射表，是上面两个入口的共同实现。
///
/// 参数：`text` 待投影的原文；`all_readings` 为 `true` 时多音字贡献全部读音的首字母，
/// 为 `false` 时只取首读音
/// 核心流程：逐字符扫过原文取 `char_indices`——汉字用 `ToPinyinMulti` 读拼音，逐个
/// `first_letter()` 追加（同一汉字内用已写入的片段去重）；没有拼音数据的字符（拉丁字母、
/// 数字、标点、假名等）把 `to_lowercase` 的每个字符原样追加（`İ` 这类会折叠成多个字符的
/// 也逐字登记）。每追加一个字符，就把它在首字母串里的字节起点与原文该字符的字节起点配成
/// 一条映射。两侧都只按 `char` 边界写入，任何区间都不含 UTF-8 的中间字节。
fn project(text: &str, all_readings: bool) -> (String, Vec<(usize, usize)>) {
    let mut initials = String::with_capacity(text.len());
    let mut map: Vec<(usize, usize)> = Vec::with_capacity(text.len());

    for (start, ch) in text.char_indices() {
        let written = initials.len();
        match ch.to_pinyin_multi() {
            Some(readings) => {
                for reading in readings {
                    let letter = reading.first_letter();
                    // 只和本字符已写入的部分比，天然完成「同一汉字内去重」。
                    if !initials[written..].contains(letter) {
                        initials.push_str(letter);
                    }
                    if !all_readings {
                        break;
                    }
                }
            }
            None => {
                for lower in ch.to_lowercase() {
                    initials.push(lower);
                }
            }
        }

        // 登记本字符新写入的每个首字母，它们都映射回同一个原文字符。
        let mut offset = written;
        while let Some(current) = initials[offset..].chars().next() {
            map.push((offset, start));
            offset += current.len_utf8();
        }
    }

    (initials, map)
}

/// 把首字母串中的一个字节位置换算回原文的字符字节区间。
///
/// 参数：`text` 原文；`map` [`initials_with_map`] 或 [`initials_with_all_readings`]
/// 返回的映射表；`initials_start` 首字母串中的字节起点（必须是某个首字母的起点）
/// 核心流程：映射表按首字母串字节起点升序排列，先二分定位起点相同的那一条；找不到
/// （起点落在某个字符中间或根本不在表里）返回 `None`。命中后用原文该字符的 `len_utf8`
/// 补出终点，得到可以直接喂给 UI 加粗的原文区间。
pub fn original_range(
    text: &str,
    map: &[(usize, usize)],
    initials_start: usize,
) -> Option<Range<usize>> {
    let index = map.partition_point(|(initial, _)| *initial < initials_start);
    let &(initial, original_start) = map.get(index)?;
    if initial != initials_start {
        return None;
    }
    let ch = text[original_start..].chars().next()?;
    Some(original_start..original_start + ch.len_utf8())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn chinese_text_maps_to_initial_letters() {
        let text = "文件";
        let (initials, map) = initials_with_map(text);

        assert_eq!(initials, "wj");
        assert_eq!(map, vec![(0, 0), (1, 3)]);
        assert_eq!(original_range(text, &map, 0), Some(0..3));
        assert_eq!(original_range(text, &map, 1), Some(3..6));
        assert_eq!(&text[0..3], "文");
        assert_eq!(&text[3..6], "件");
    }

    #[test]
    fn polyphonic_characters_expose_every_reading() {
        let text = "音乐";

        // 首读音优先：乐 读 lè，所以默认首字母串是 yl。
        assert_eq!(initials_with_map(text).0, "yl");

        // 放开全部读音后 乐 还贡献 yuè 的 y，`yy` 才可能命中「音乐」。
        let (initials, map) = initials_with_all_readings(text);
        assert_eq!(initials, "yly");
        assert_eq!(map, vec![(0, 0), (1, 3), (2, 3)]);
        // 同一汉字的多个首字母共享一段原文区间，仍然落在字符边界上。
        assert_eq!(original_range(text, &map, 1), Some(3..6));
        assert_eq!(original_range(text, &map, 2), Some(3..6));
    }

    #[test]
    fn mixed_text_keeps_ascii_and_maps_chinese() {
        let text = "ai-code [hint] 文件";
        let (initials, map) = initials_with_map(text);

        assert_eq!(initials, "ai-code [hint] wj");
        // 前面的拉丁与标点逐字节原样保留，映射指向自己。
        for (initial_start, original_start) in map.iter().take(15) {
            assert_eq!(initial_start, original_start, "拉丁部分应保持原样");
        }
        // 中文部分仍然能换算回原文的两段。
        assert_eq!(original_range(text, &map, 15), Some(15..18));
        assert_eq!(original_range(text, &map, 16), Some(18..21));
        assert_eq!(&text[15..18], "文");
        assert_eq!(&text[18..21], "件");
    }

    #[test]
    fn empty_and_ascii_text_are_harmless() {
        for project in [initials_with_map, initials_with_all_readings] {
            assert_eq!(project(""), (String::new(), Vec::new()));

            let (initials, map) = project("PowerPoint");
            assert_eq!(initials, "powerpoint");
            assert_eq!(map.len(), initials.chars().count());
            assert_eq!(original_range("PowerPoint", &map, 0), Some(0..1));
            assert_eq!(original_range("PowerPoint", &map, 10), None);
        }
    }

    #[test]
    fn unknown_positions_do_not_panic() {
        let text = "文件";
        let (_, map) = initials_with_map(text);

        // 起点不是任何首字母的起点、越界、空表都只返回 `None`。
        assert_eq!(original_range(text, &map, 2), None);
        assert_eq!(original_range(text, &map, 99), None);
        assert_eq!(original_range("", &[], 0), None);
    }
}
