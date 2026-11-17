//! Fallback for platforms with neither process groups nor job objects: kill
//! the direct child only, and accept that a grandchild may survive.

use std::process::{Child, Command};

pub fn configure(_cmd: &mut Command) {}

pub fn kill_tree(child: &mut Child) {
    let _ = child.kill();
}

/// No process group exists to record on this platform, so `stop --force` has
/// nothing extra to kill and keeps its ordinary behaviour.
pub fn record_child(_pid: u32) -> Result<(), String> {
    Ok(())
}

/// Nothing was recorded, so there is nothing to remove.
pub fn clear_child() -> Result<(), String> {
    Ok(())
}

/// No record exists on this platform; see [`record_child`].
pub fn kill_recorded_tree(_eval_pid: u32) -> Result<bool, String> {
    Ok(false)
}
