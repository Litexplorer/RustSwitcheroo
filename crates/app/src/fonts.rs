//! 系统中文系统字体注入 + 加粗字体族注册（US-042）。
//!
//! 参数：`ctx` egui 上下文
//! 核心流程：egui 自带字体不含中日韩字形，改为运行时按顺序探测 Windows 自带中文字体
//! （微软雅黑 → 黑体 → 宋体），把第一个能读到的一份追加到 Proportional 与 Monospace
//! 回退链末尾（拉丁字符仍用内置字体，只补缺字）；粗体族优先用雅黑 Bold，读不到就用同一份
//! 普通中文字体（字形本身够重，视觉可接受）注册名为 `bold` 的族；系统字体全找不到时不注册
//! 额外字体，`bold_family` 回退到默认比例族，界面仍可运行，绝不 panic。

use eframe::egui;
use std::sync::{Arc, OnceLock};

/// 普通中文字体在 `FontDefinitions::font_data` 里的名字。
const FONT_DATA_NAME: &str = "cjk";
/// 雅黑 Bold 在 `FontDefinitions::font_data` 里的名字（找不到时复用 [`FONT_DATA_NAME`]）。
const BOLD_DATA_NAME: &str = "cjk-bold";
/// 加粗族名：US-042 的命中段用 `FontFamily::Name("bold")` 渲染。
const BOLD_FAMILY_NAME: &str = "bold";

/// `bold` 族是否真的注册成功。
///
/// epaint 用到没有绑定字体的族时会直接 panic（"FontFamily is not bound to any fonts"），
/// 所以没找到中文字体时不能把 `bold` 族挂上去，[`bold_family`] 据此回退到默认族。
static BOLD_REGISTERED: OnceLock<bool> = OnceLock::new();

/// 一个候选系统字体：文件路径 + 字体集合（`.ttc`）里的 face 序号。
#[derive(Clone, Copy)]
struct Candidate {
    /// 字体文件路径。
    path: &'static str,
    /// face 序号；`.ttf` 只有一个 face，`.ttc` 的第 0 个 face 也是 0。
    index: u32,
}

/// 普通字体候选，按优先级从高到低探测（雅黑最能保持界面观感一致）。
const REGULAR_CANDIDATES: &[Candidate] = &[
    Candidate {
        path: r"C:\Windows\Fonts\msyh.ttc",
        index: 0,
    },
    Candidate {
        path: r"C:\Windows\Fonts\simhei.ttf",
        index: 0,
    },
    Candidate {
        path: r"C:\Windows\Fonts\simsun.ttc",
        index: 0,
    },
];

/// 粗体候选：雅黑 Bold。
const BOLD_CANDIDATE: Candidate = Candidate {
    path: r"C:\Windows\Fonts\msyhbd.ttc",
    index: 0,
};

/// 参数：`ctx` egui 上下文
/// 核心流程：按 [`REGULAR_CANDIDATES`] 顺序探测系统字体 → 注册为 Proportional/Monospace
/// 的回退项，再按 [`BOLD_CANDIDATE`] 注册 `bold` 族 → 交回 egui；返回是否读到中文字体
/// （全找不到时不碰字体定义、不注册 `bold` 族，返回 false）。
pub fn install(ctx: &egui::Context) -> bool {
    install_from(ctx, REGULAR_CANDIDATES, BOLD_CANDIDATE)
}

/// 参数：无
/// 核心流程：返回命中高亮要用的字体族——`bold` 族注册成功时用它，否则（没找到中文字体）
/// 回退到默认比例族；字体缺失时高亮段只是不加粗，绝不会 panic。
pub fn bold_family() -> egui::FontFamily {
    bold_family_from(BOLD_REGISTERED.get().copied().unwrap_or_default())
}

/// 参数：`registered` `bold` 族是否已注册
/// 核心流程：把注册状态映射成字体族——已注册返回 `bold` 族，否则回退默认比例族。
/// 抽成纯函数便于跨平台单测（不依赖本机是否真的装了中文字体）。
fn bold_family_from(registered: bool) -> egui::FontFamily {
    if registered {
        egui::FontFamily::Name(BOLD_FAMILY_NAME.into())
    } else {
        egui::FontFamily::Proportional
    }
}

/// 参数：`ctx` egui 上下文，`regular_candidates` 普通字体候选（按序探测），
/// `bold_candidate` 粗体候选
/// 核心流程：探测普通字体 → 追加到 Proportional/Monospace 回退链末尾 → 探测粗体字体
/// （读不到就复用普通字体）注册 `bold` 族 → 应用到上下文；普通字体全找不到时返回 false。
fn install_from(
    ctx: &egui::Context,
    regular_candidates: &[Candidate],
    bold_candidate: Candidate,
) -> bool {
    let Some((regular, regular_data)) = load_first(regular_candidates) else {
        log::warn!("未找到系统中文字体，中文可能显示为方块");
        // 没有字形数据就不注册 `bold` 族：否则渲染命中段时会命中 epaint 的 panic。
        let _ = BOLD_REGISTERED.set(false);
        return false;
    };

    let mut fonts = egui::FontDefinitions::default();
    // 默认比例族的回退链（egui 自带字体）：给 `bold` 族当兜底，中文字体缺字
    //（例如 emoji）时不会画成方块。
    let default_chain = fonts
        .families
        .get(&egui::FontFamily::Proportional)
        .cloned()
        .unwrap_or_default();

    log::info!(
        "使用系统中文字体：{}（face {}）",
        regular.path,
        regular.index
    );
    fonts
        .font_data
        .insert(FONT_DATA_NAME.to_owned(), Arc::new(regular_data));

    // 追加到回退链末尾 -> 拉丁字符仍用默认字体，缺字时才落到中文字体
    for family in [egui::FontFamily::Proportional, egui::FontFamily::Monospace] {
        fonts
            .families
            .entry(family)
            .or_default()
            .push(FONT_DATA_NAME.to_owned());
    }

    // 粗体族：雅黑 Bold 读到就用它，否则复用普通中文字体（字形本身够重，观感可接受）。
    let bold_source = match load(bold_candidate) {
        Some(data) => {
            log::info!(
                "粗体使用系统中文字体：{}（face {}）",
                bold_candidate.path,
                bold_candidate.index
            );
            fonts
                .font_data
                .insert(BOLD_DATA_NAME.to_owned(), Arc::new(data));
            BOLD_DATA_NAME
        }
        None => {
            log::info!(
                "未找到雅黑 Bold（{}），改用普通中文字体充当粗体族",
                bold_candidate.path
            );
            FONT_DATA_NAME
        }
    };

    let mut bold_chain = vec![bold_source.to_owned()];
    bold_chain.extend(default_chain);
    fonts
        .families
        .insert(egui::FontFamily::Name(BOLD_FAMILY_NAME.into()), bold_chain);

    ctx.set_fonts(fonts);
    let _ = BOLD_REGISTERED.set(true);
    true
}

/// 参数：`candidates` 候选列表
/// 核心流程：按顺序逐个尝试读取，返回第一个读成功的候选及其字体数据；全部失败返回 `None`。
fn load_first(candidates: &[Candidate]) -> Option<(Candidate, egui::FontData)> {
    candidates
        .iter()
        .find_map(|candidate| load(*candidate).map(|data| (*candidate, data)))
}

/// 参数：`candidate` 待读取的候选字体
/// 核心流程：`std::fs::read` 读整份字体字节包成 `FontData`，并把 `.ttc` 的 face 序号
/// 写进 `index` 字段；文件不存在或读取失败返回 `None`，由调用方继续探测下一个候选。
fn load(candidate: Candidate) -> Option<egui::FontData> {
    let bytes = std::fs::read(candidate.path).ok()?;
    Some(egui::FontData {
        index: candidate.index,
        ..egui::FontData::from_owned(bytes)
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    /// 一个必然读不到的相对路径，用来模拟"本机没有中文字体"。
    fn missing_candidate() -> Candidate {
        Candidate {
            path: "__switcheroo_no_such_font__.ttf",
            index: 0,
        }
    }

    /// 参数：无
    /// 核心流程：读取不存在的文件应返回 `None` 而不是 panic。
    #[test]
    fn load_returns_none_for_missing_file() {
        assert!(load(missing_candidate()).is_none());
    }

    /// 参数：无
    /// 核心流程：候选全部读不到时探测结果为 `None`（对应"没找到中文字体"）。
    #[test]
    fn load_first_returns_none_when_all_candidates_missing() {
        let candidates = [missing_candidate(), missing_candidate()];
        assert!(load_first(&candidates).is_none());
    }

    /// 参数：无
    /// 核心流程：没有可用字体时 `install` 返回 false，且不注册 `bold` 族（不 panic）。
    #[test]
    fn install_returns_false_without_available_font() {
        let ctx = egui::Context::default();
        let empty_chain = [missing_candidate()];
        assert!(!install_from(&ctx, &empty_chain, missing_candidate()));
    }

    /// 参数：无
    /// 核心流程：未注册 `bold` 族时回退到默认比例族，注册后指向 `bold` 族。
    #[test]
    fn bold_family_falls_back_to_proportional() {
        assert_eq!(bold_family_from(false), egui::FontFamily::Proportional);
        assert_eq!(
            bold_family_from(true),
            egui::FontFamily::Name(BOLD_FAMILY_NAME.into())
        );
    }
}
