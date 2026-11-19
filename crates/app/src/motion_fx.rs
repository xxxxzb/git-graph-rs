//! 原生 macOS 质感物理动效状态机与校验辅助函数。
//!
//! 包含：
//! - [`RubberBandSpring`]：四向触边阻尼过冲累加与 `ely_core::motion::spring` 弹性回弹模型（静止后零帧请求）。
//! - 模态弹窗输入校验辅助函数（驱动 `Shake` 计数与提示文案）。

use std::time::{Duration, Instant};

use ely_core::motion::spring;

/// 弹性释放动画片段（记录起跳偏移与起止时间）。
#[derive(Debug, Clone, Copy, PartialEq)]
struct SpringRelease {
    start_offset: f32,
    started_at: Instant,
    duration: Duration,
}

/// 纯数学物理橡皮筋阻尼回弹模型。
///
/// - 拉动阶段使用饱和阻尼曲线 `max_pull * (1 - exp(-|raw| / k))`；
/// - 松手（`TouchPhase::Ended`）或超过 [`Self::IDLE_RELEASE_TIMEOUT`] 无新滚轮事件后，
///   使用 [`ely_core::motion::spring`] 在 [`Self::SPRING_DURATION`] 内平滑回弹归零；
/// - 归零后 [`Self::is_animating`] 立即返回 `false`，绝不产生后台空转帧循环；
/// - 开启 `reduced_motion` 时立即归零，不做弹性晃动。
#[derive(Debug, Clone, PartialEq)]
pub struct RubberBandSpring {
    max_pull: f32,
    stiffness_k: f32,
    raw_pull: f32,
    offset: f32,
    last_pull_at: Option<Instant>,
    release: Option<SpringRelease>,
}

impl Default for RubberBandSpring {
    fn default() -> Self {
        Self::new(Self::DEFAULT_MAX_PULL_PX)
    }
}

impl RubberBandSpring {
    /// 默认最大过冲距离（px）。
    pub const DEFAULT_MAX_PULL_PX: f32 = 28.0;
    /// 默认阻尼系数 `k`。
    pub const DEFAULT_STIFFNESS_K: f32 = 56.0;
    /// 超过该间隔无新滚轮事件时自动触发回弹（ms）。
    pub const IDLE_RELEASE_TIMEOUT: Duration = Duration::from_millis(120);
    /// 弹簧回弹总时长（ms）。
    pub const SPRING_DURATION: Duration = Duration::from_millis(260);

    /// 创建指定最大过冲上限（px）的橡皮筋弹簧模型。
    pub fn new(max_pull: f32) -> Self {
        Self {
            max_pull: max_pull.max(1.0),
            stiffness_k: Self::DEFAULT_STIFFNESS_K,
            raw_pull: 0.0,
            offset: 0.0,
            last_pull_at: None,
            release: None,
        }
    }

    /// 当前弹性偏移量（px，范围 `[-max_pull, +max_pull]`）。
    pub fn offset(&self) -> f32 {
        self.offset
    }

    /// 当前是否正处于弹簧回弹动画帧循环中。
    pub fn is_animating(&self) -> bool {
        self.release.is_some()
    }

    /// 当前是否处于任意非静止状态（拉动中、待释放或回弹中）。
    pub fn is_active(&self) -> bool {
        self.is_animating() || self.last_pull_at.is_some() || self.offset.abs() > 0.001
    }

    /// 判断在 `now` 时刻是否已超过无事件超时阈值而需要开始回弹。
    pub fn needs_idle_release(&self, now: Instant) -> bool {
        self.release.is_none()
            && self
                .last_pull_at
                .is_some_and(|at| now.saturating_duration_since(at) >= Self::IDLE_RELEASE_TIMEOUT)
    }

    /// 阻尼映射函数：`sign(raw) * max_pull * (1 - exp(-|raw| / k))`。
    fn resistance(&self, raw: f32) -> f32 {
        if raw.abs() <= f32::EPSILON {
            return 0.0;
        }
        let mag = self.max_pull * (1.0 - (-raw.abs() / self.stiffness_k).exp());
        raw.signum() * mag.min(self.max_pull)
    }

    /// 由当前视觉 `offset` 反推等效 `raw_pull`，保证回弹中途再次抓拉或反向抵消时手感连续。
    fn inverse_resistance(&self, offset: f32) -> f32 {
        let ratio = (offset.abs() / self.max_pull).clamp(0.0, 0.995);
        if ratio <= f32::EPSILON {
            return 0.0;
        }
        offset.signum() * (-self.stiffness_k * (1.0 - ratio).ln())
    }

    /// 立即重置为静止零位。
    pub fn reset(&mut self) {
        self.raw_pull = 0.0;
        self.offset = 0.0;
        self.last_pull_at = None;
        self.release = None;
    }

    /// 在指定时刻 `now` 累加过冲增量并更新阻尼偏移。
    pub fn pull_at(&mut self, delta: f32, now: Instant, reduced_motion: bool) -> f32 {
        if reduced_motion {
            self.reset();
            return 0.0;
        }
        if !delta.is_finite() || delta == 0.0 {
            return self.offset;
        }
        if self.release.is_some() {
            self.raw_pull = self.inverse_resistance(self.offset);
            self.release = None;
        }
        let raw_cap = self.max_pull * 8.0;
        self.raw_pull = (self.raw_pull + delta).clamp(-raw_cap, raw_cap);
        self.last_pull_at = Some(now);
        self.offset = self.resistance(self.raw_pull);
        self.offset
    }

    /// 当用户向回滑动（`delta` 与当前过冲方向相反）时，先抵消既有过冲并返回剩余未消费的 `delta`。
    pub fn consume_opposite(&mut self, delta: f32, now: Instant, reduced_motion: bool) -> f32 {
        if !delta.is_finite() || delta == 0.0 {
            return delta;
        }
        if reduced_motion {
            self.reset();
            return delta;
        }
        if self.offset.abs() <= 0.001 {
            self.reset();
            return delta;
        }
        if self.offset * delta >= 0.0 {
            return delta;
        }
        let effective_raw = self.inverse_resistance(self.offset);
        self.release = None;
        if delta.abs() <= effective_raw.abs() {
            self.raw_pull = effective_raw + delta;
            self.last_pull_at = Some(now);
            self.offset = self.resistance(self.raw_pull);
            0.0
        } else {
            let remaining = delta + effective_raw;
            self.reset();
            remaining
        }
    }

    /// 松手 / 停止拉动，触发弹簧回弹（使用当前时间戳）。
    pub fn release(&mut self, reduced_motion: bool) {
        self.release_at(Instant::now(), reduced_motion);
    }

    /// 在指定时刻 `now` 触发弹簧回弹；`reduced_motion` 为 `true` 时立刻归零。
    pub fn release_at(&mut self, now: Instant, reduced_motion: bool) {
        self.last_pull_at = None;
        self.raw_pull = 0.0;
        if reduced_motion || self.offset.abs() < 0.05 {
            self.offset = 0.0;
            self.release = None;
            return;
        }
        self.release = Some(SpringRelease {
            start_offset: self.offset,
            started_at: now,
            duration: Self::SPRING_DURATION,
        });
    }

    /// 推进一帧弹性动画状态，返回是否仍需要下一帧（归零后返回 `false`）。
    pub fn tick(&mut self, reduced_motion: bool) -> bool {
        self.tick_at(Instant::now(), reduced_motion)
    }

    /// 在指定时刻 `now` 推进弹性动画状态，返回是否仍需要下一帧。
    pub fn tick_at(&mut self, now: Instant, reduced_motion: bool) -> bool {
        if reduced_motion {
            self.reset();
            return false;
        }
        if self.needs_idle_release(now) {
            self.release_at(now, false);
        }
        let Some(rel) = self.release else {
            return false;
        };
        let elapsed = now.saturating_duration_since(rel.started_at);
        let t = (elapsed.as_secs_f32() / rel.duration.as_secs_f32().max(f32::EPSILON))
            .clamp(0.0, 1.0);
        if t >= 1.0 {
            self.offset = 0.0;
            self.raw_pull = 0.0;
            self.release = None;
            return false;
        }
        self.offset = rel.start_offset * (1.0 - spring(t));
        true
    }
}

/// 校验 Git 分支或标签通用引用名（空、空白、`-` 开头及 Git 禁止字符）。
fn validate_git_ref_name(name: &str, kind_label: &str) -> Result<(), &'static str> {
    let trimmed = name.trim();
    if trimmed.is_empty() {
        return Err(match kind_label {
            "标签" => "标签名不能为空",
            "远程分支" => "远程分支名不能为空",
            _ => "分支名称不能为空",
        });
    }
    if trimmed.chars().any(char::is_whitespace) {
        return Err("名称不能包含空格或空白字符");
    }
    if trimmed.starts_with('-') {
        return Err("名称不能以 '-' 开头");
    }
    if trimmed.starts_with('/')
        || trimmed.ends_with('/')
        || trimmed.ends_with('.')
        || trimmed.ends_with(".lock")
        || trimmed.contains("..")
        || trimmed.contains("//")
        || trimmed.contains("@{")
        || trimmed.chars().any(|c| matches!(c, '~' | '^' | ':' | '?' | '*' | '[' | '\\'))
    {
        return Err("名称包含 Git 不允许的特殊字符或格式");
    }
    Ok(())
}

/// 校验「创建分支」弹窗输入。
pub fn validate_create_branch_name(name: &str) -> Result<(), &'static str> {
    validate_git_ref_name(name, "分支")
}

/// 校验「重命名分支」弹窗输入（额外检查不可与原分支同名）。
pub fn validate_rename_branch_name(old_name: &str, new_name: &str) -> Result<(), &'static str> {
    validate_git_ref_name(new_name, "分支")?;
    if new_name.trim() == old_name.trim() {
        return Err("新分支名称不能与原分支相同");
    }
    Ok(())
}

/// 校验「创建标签」弹窗输入。
pub fn validate_tag_name(name: &str) -> Result<(), &'static str> {
    validate_git_ref_name(name, "标签")
}

/// 校验「推送分支」弹窗输入。
pub fn validate_push_branch_inputs(
    local_branch: &str,
    remote: &str,
    remote_branch: &str,
) -> Result<(), &'static str> {
    if local_branch.trim().is_empty() {
        return Err("请选择要推送的本地分支");
    }
    if remote.trim().is_empty() {
        return Err("请先配置并选择目标远端 (Remote)");
    }
    validate_git_ref_name(remote_branch, "远程分支")
}

/// 递增模态弹窗校验失败轻晃序列号（供 `Shake` 绑定键使用）。
pub fn next_shake_serial(serial: &mut u64) -> u64 {
    *serial = serial.saturating_add(1);
    *serial
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn rubber_band_saturates_under_max_pull_and_rebounds_to_zero() {
        let mut spring = RubberBandSpring::new(28.0);
        let t0 = Instant::now();

        // 连续大幅向左/向右过冲累加，永远严格饱和在 [-28.0, 28.0] 内
        for _ in 0..20 {
            let off = spring.pull_at(40.0, t0, false);
            assert!(off > 0.0 && off <= 28.0, "positive offset out of bounds: {off}");
        }
        assert!(spring.offset() > 26.0 && spring.offset() <= 28.0);
        assert!(spring.is_active());
        assert!(!spring.is_animating());

        // 释放后启动 ~260ms 弹性回弹
        spring.release_at(t0, false);
        assert!(spring.is_animating());

        // 中途某帧在回弹曲线上
        let mid_still = spring.tick_at(t0 + Duration::from_millis(80), false);
        assert!(mid_still);
        assert!(spring.is_animating());

        // 超过 260ms 后精确归零，并彻底停止动画（无帧泄漏）
        let end_still = spring.tick_at(t0 + Duration::from_millis(260), false);
        assert!(!end_still);
        assert!(!spring.is_animating());
        assert!(!spring.is_active());
        assert_eq!(spring.offset(), 0.0);

        // 负向（底部触底或右侧过冲）同样饱和在 [-28.0, 0) 并正确归零
        let neg = spring.pull_at(-200.0, t0, false);
        assert!((-28.0..0.0).contains(&neg));
        spring.release_at(t0, false);
        assert!(!spring.tick_at(t0 + Duration::from_millis(300), false));
        assert_eq!(spring.offset(), 0.0);
    }

    #[test]
    fn rubber_band_auto_releases_after_idle_timeout_and_respects_reduced_motion() {
        let mut spring = RubberBandSpring::new(28.0);
        let t0 = Instant::now();
        spring.pull_at(35.0, t0, false);
        assert!(!spring.is_animating());

        // 未到 120ms 超时不会提前释放
        assert!(!spring.tick_at(t0 + Duration::from_millis(60), false));
        assert!(!spring.is_animating());

        // 超过 120ms 无新事件后自动进入弹簧回弹
        assert!(spring.tick_at(t0 + Duration::from_millis(125), false));
        assert!(spring.is_animating());

        // reduced_motion=true 时立即归零，绝不继续请求动画帧
        assert!(!spring.tick_at(t0 + Duration::from_millis(140), true));
        assert!(!spring.is_animating());
        assert_eq!(spring.offset(), 0.0);

        // 在 reduced_motion=true 下拉动也会立即归零
        assert_eq!(spring.pull_at(50.0, t0, true), 0.0);
        assert!(!spring.is_active());
    }

    #[test]
    fn rubber_band_bottom_bounce_damping_and_opposite_consume() {
        let mut bottom = RubberBandSpring::new(20.0);
        let t0 = Instant::now();
        let off = bottom.pull_at(-80.0, t0, false);
        assert!((-20.0..0.0).contains(&off));

        // 向反方向滑回：部分抵消时剩余 delta 为 0
        let rem1 = bottom.consume_opposite(10.0, t0, false);
        assert_eq!(rem1, 0.0);
        assert!(bottom.offset() < 0.0);

        // 大幅向反方向滑回：完全抵消后返回多余增量，继续向顶部过冲同样在 +20px 内饱和
        let rem2 = bottom.consume_opposite(500.0, t0, false);
        assert!(rem2 > 0.0);
        assert_eq!(bottom.offset(), 0.0);
        assert!(!bottom.is_active());

        let top_pull = bottom.pull_at(400.0, t0, false);
        assert!(top_pull > 0.0 && top_pull <= 20.0);
        bottom.release_at(t0, false);
        assert!(!bottom.tick_at(t0 + Duration::from_millis(300), false));
        assert_eq!(bottom.offset(), 0.0);
    }

    #[test]
    fn modal_validation_helpers_reject_illegal_inputs() {
        assert!(validate_create_branch_name("").is_err());
        assert!(validate_create_branch_name("feat bad").is_err());
        assert!(validate_create_branch_name("-bad").is_err());
        assert!(validate_create_branch_name("feat..bad").is_err());
        assert!(validate_create_branch_name("feat/motion-fx").is_ok());

        assert!(validate_rename_branch_name("main", "main").is_err());
        assert!(validate_rename_branch_name("main", "develop").is_ok());

        assert!(validate_tag_name("v 1.0").is_err());
        assert!(validate_tag_name("v1.0.0").is_ok());

        assert!(validate_push_branch_inputs("main", "", "main").is_err());
        assert!(validate_push_branch_inputs("main", "origin", "").is_err());
        assert!(validate_push_branch_inputs("main", "origin", "main").is_ok());

        let mut shake_serial = 0_u64;
        assert_eq!(next_shake_serial(&mut shake_serial), 1);
        assert_eq!(next_shake_serial(&mut shake_serial), 2);
        assert_eq!(shake_serial, 2);
    }
}
