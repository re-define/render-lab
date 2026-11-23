use crate::rhi::ManagedMemory;
use anyhow::{Context, Result, ensure};
use std::fmt::Write;

pub(super) fn describe(memory: Option<&ManagedMemory>, arg: &str) -> Result<String> {
    let (mode, filter) = arg.split_once(char::is_whitespace).unwrap_or((arg, ""));
    ensure!(
        matches!(mode, "" | "idle" | "alloc" | "driver"),
        "memory idle/alloc/driver [NAME or #OWNER]"
    );
    let filter = filter.trim().to_lowercase();
    let owner = filter
        .strip_prefix('#')
        .map(str::parse::<u64>)
        .transpose()
        .context("expected #OWNER")?;
    let Some(m) = memory else {
        return Ok("等待第一帧显存统计".into());
    };
    let mut rows: Vec<_> = m
        .purposes
        .iter()
        .filter(|p| {
            if let Some(owner) = owner {
                p.owner == Some(owner)
            } else {
                p.name.to_lowercase().contains(&filter)
            }
        })
        .collect();
    rows.sort_by_key(|p| {
        std::cmp::Reverse(match mode {
            "alloc" => (p.frame_bindings.allocated_bytes, p.bindings.allocated_bytes),
            "driver" => (p.frame_driver.allocated_bytes, p.driver.allocated_bytes),
            _ => (p.unused_bytes, p.capacity_bytes),
        })
    });
    let mib = |n: u64| n as f64 / (1 << 20) as f64;
    let mut text = format!(
        "提交 {} · 设备本地 {:.1} MiB · {} 个内存块 · 块内可分配 {:.1} MiB",
        m.submission,
        mib(m.device_local.total()),
        m.pool_blocks,
        mib(m.device_local.pool_free + m.nonlocal.pool_free)
    );
    if rows.is_empty() {
        text.push_str("\n没有匹配的资源用途");
    }
    for p in rows.into_iter().take(6) {
        write!(text, "\n{}", p.name)?;
        if let Some(id) = p.owner {
            write!(text, " #{id}")?;
        }
        if let Some(slot) = p.slot {
            write!(text, " 槽{slot}")?;
        }
        write!(
            text,
            " {}堆{}",
            if p.device_local {
                "本地"
            } else {
                "非本地"
            },
            p.heap_index
        )?;
        write!(
            text,
            " · 有效/容量/闲置 {:.2}/{:.2}/{:.2} MiB · 最近扩容 {} · 本帧绑定 {}/{:.2} MiB · 驱动 {}/{:.2} MiB",
            mib(p.used_bytes),
            mib(p.capacity_bytes),
            mib(p.unused_bytes),
            p.last_growth_submission.unwrap_or_default(),
            p.frame_bindings.allocations,
            mib(p.frame_bindings.allocated_bytes),
            p.frame_driver.allocations,
            mib(p.frame_driver.allocated_bytes)
        )?;
        if mode == "alloc" || mode == "driver" {
            let total = if mode == "alloc" {
                &p.bindings
            } else {
                &p.driver
            };
            write!(
                text,
                " · 累计 {} 次/{:.2} MiB",
                total.allocations,
                mib(total.allocated_bytes)
            )?;
        }
    }
    Ok(text)
}
