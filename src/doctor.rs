//! Checks whether this machine can measure benchmarks reliably.
//!
//! Always informational — see [`run`]. Every finding names a way round
//! timings can be quietly different from what a reader assumes: a CPU still
//! ramping its clock, a laptop running on battery, an incremental build
//! cache, a nightly toolchain with different codegen than the stable a
//! reader would assume. The human decides whether to fix it or read the
//! numbers skeptically; nothing here gates anything.

use std::path::{Path, PathBuf};

/// One diagnostic finding.
///
/// `ok` is for the human reading the output, never for scripting a gate on
/// top of it — [`run`]'s caller always exits 0 regardless of what any
/// individual check found.
pub struct Check {
    pub name: String,
    pub ok: bool,
    pub detail: String,
}

fn check(name: &str, ok: bool, detail: impl Into<String>) -> Check {
    Check {
        name: name.to_string(),
        ok,
        detail: detail.into(),
    }
}

/// Runs every check. `root` is the repository root, used for the disk-space
/// and `[profile.bench]` checks.
pub fn run(root: &Path) -> Vec<Check> {
    vec![
        cpu_frequency_scaling(),
        thermal_throttle_risk(),
        load_average(),
        disk_space(root),
        on_battery(),
        cargo_incremental(),
        bench_profile_opt_level(root),
        dependency_cooldown(root),
        nightly_default_toolchain(root),
        toolchain_age(),
    ]
}

// --- CPU frequency scaling -------------------------------------------------

fn cpu_frequency_scaling() -> Check {
    if cfg!(target_os = "linux") {
        cpu_frequency_scaling_at(Path::new(
            "/sys/devices/system/cpu/cpu0/cpufreq/scaling_governor",
        ))
    } else {
        check(
            "cpu frequency scaling",
            true,
            "not checked on this platform",
        )
    }
}

/// Injectable by path so the parsing and severity logic can be exercised on
/// any host platform against a synthetic file, not just the real
/// `/sys/devices/.../scaling_governor`.
fn cpu_frequency_scaling_at(path: &Path) -> Check {
    match std::fs::read_to_string(path) {
        Ok(s) => {
            let governor = s.trim();
            let ok = governor == "performance";
            check(
                "cpu frequency scaling",
                ok,
                if ok {
                    format!("governor: {governor}")
                } else {
                    format!(
                        "governor: {governor} — round timings may drift as the CPU ramps clock \
                         speed mid-run; consider the \"performance\" governor while measuring"
                    )
                },
            )
        }
        // Missing cpufreq is normal on a VM or container without frequency
        // scaling at all; the check simply did not run, which counts as
        // fine rather than as a finding.
        Err(_) => check(
            "cpu frequency scaling",
            true,
            "not checked: cpufreq unavailable (VM, container, or non-Linux)",
        ),
    }
}

// --- Thermal throttle risk --------------------------------------------------

fn thermal_throttle_risk() -> Check {
    if cfg!(target_os = "linux") {
        thermal_throttle_risk_at(Path::new(
            "/sys/devices/system/cpu/cpu0/thermal_throttle/core_throttle_count",
        ))
    } else if cfg!(target_os = "macos") {
        match std::process::Command::new("pmset")
            .args(["-g", "therm"])
            .output()
        {
            Ok(out) if out.status.success() => {
                thermal_throttle_from_pmset(&String::from_utf8_lossy(&out.stdout))
            }
            _ => check(
                "thermal throttle risk",
                true,
                "not checked: `pmset -g therm` unavailable",
            ),
        }
    } else {
        check(
            "thermal throttle risk",
            true,
            "not checked on this platform",
        )
    }
}

/// Injectable by path, mirroring [`cpu_frequency_scaling_at`].
fn thermal_throttle_risk_at(path: &Path) -> Check {
    match std::fs::read_to_string(path) {
        Ok(s) => match s.trim().parse::<u64>() {
            Ok(0) => check(
                "thermal throttle risk",
                true,
                "no throttling events recorded since boot",
            ),
            Ok(n) => check(
                "thermal throttle risk",
                false,
                format!(
                    "{n} thermal-throttle event(s) recorded since boot — round timings may be \
                     depressed by heat"
                ),
            ),
            Err(_) => check(
                "thermal throttle risk",
                true,
                "not checked: unexpected content in the throttle counter",
            ),
        },
        Err(_) => check(
            "thermal throttle risk",
            true,
            "not checked: thermal-throttle counter unavailable",
        ),
    }
}

/// Parses `pmset -g therm`'s `CPU_Speed_Limit` line. macOS clamps this below
/// 100 once it has started throttling the CPU to manage heat.
fn thermal_throttle_from_pmset(s: &str) -> Check {
    for line in s.lines() {
        if let Some(rest) = line.split("CPU_Speed_Limit").nth(1) {
            // `rest` looks like "\t= 75" or " = 100": normalize every
            // separator (tabs, spaces, '=') to a single space so the first
            // whitespace-separated token is always the number itself.
            let normalized = rest.replace(['\t', '='], " ");
            if let Some(num) = normalized.split_whitespace().next() {
                if let Ok(pct) = num.trim_end_matches('%').parse::<u32>() {
                    let ok = pct >= 100;
                    return check(
                        "thermal throttle risk",
                        ok,
                        if ok {
                            format!("CPU_Speed_Limit {pct}%")
                        } else {
                            format!(
                                "CPU_Speed_Limit {pct}% — already throttled; round timings will \
                                 read slower than an unthrottled machine's"
                            )
                        },
                    );
                }
            }
        }
    }
    check(
        "thermal throttle risk",
        true,
        "not checked: could not parse `pmset -g therm`",
    )
}

// --- Load average ------------------------------------------------------------

/// The severity logic, separated from the syscall so it can be tested
/// without depending on the real machine's current load.
///
/// Unix-only: its only caller, [`load_average`], only exists on unix (there
/// is no portable `getloadavg` equivalent), so this would otherwise be dead
/// code on every other platform.
#[cfg(unix)]
fn load_average_finding(avg: f64, cpus: f64) -> Check {
    let threshold = 0.5 * cpus;
    let ok = avg <= threshold;
    check(
        "load average",
        ok,
        format!(
            "1-minute load average {avg:.2} (threshold {threshold:.2} for {cpus:.0} logical \
             cpus){}",
            if ok {
                ""
            } else {
                " — background load will show up as noise in every measurement"
            }
        ),
    )
}

#[cfg(unix)]
fn load_average() -> Check {
    let mut avg = [0.0f64; 1];
    // SAFETY: `avg` is a valid, correctly-sized buffer for one sample.
    let n = unsafe { libc::getloadavg(avg.as_mut_ptr(), 1) };
    if n < 1 {
        return check("load average", true, "not checked: getloadavg failed");
    }
    let cpus = std::thread::available_parallelism()
        .map(|n| n.get())
        .unwrap_or(1) as f64;
    load_average_finding(avg[0], cpus)
}

#[cfg(not(unix))]
fn load_average() -> Check {
    check("load average", true, "not checked on this platform")
}

// --- Disk space ---------------------------------------------------------------

/// Unix-only: its only caller, [`disk_space`], only exists on unix (it is
/// built on `statvfs`), so this would otherwise be dead code on every other
/// platform.
#[cfg(unix)]
fn disk_space_finding(available_bytes: u64) -> Check {
    let gb = available_bytes as f64 / (1024.0 * 1024.0 * 1024.0);
    const MIN_GB_WARN: f64 = 2.0;
    let ok = gb >= MIN_GB_WARN;
    check(
        "disk space",
        ok,
        if ok {
            format!("{gb:.1} GB available")
        } else {
            format!(
                "{gb:.1} GB available — a criterion run writes a sample per round and can fail \
                 outright if the disk fills mid-run"
            )
        },
    )
}

#[cfg(unix)]
fn disk_space(root: &Path) -> Check {
    use std::os::unix::ffi::OsStrExt;
    let cpath = match std::ffi::CString::new(root.as_os_str().as_bytes()) {
        Ok(c) => c,
        Err(_) => return check("disk space", true, "not checked: path contains a NUL byte"),
    };
    // SAFETY: `buf` is zero-initialized and `statvfs` only ever writes to it
    // through the pointer it is given; POD struct, no interior pointers.
    let mut buf: libc::statvfs = unsafe { std::mem::zeroed() };
    let rc = unsafe { libc::statvfs(cpath.as_ptr(), &mut buf) };
    if rc != 0 {
        return check("disk space", true, "not checked: statvfs failed");
    }
    let available_bytes = buf.f_frsize as u64 * buf.f_bavail as u64;
    disk_space_finding(available_bytes)
}

#[cfg(not(unix))]
fn disk_space(_root: &Path) -> Check {
    check("disk space", true, "not checked on this platform")
}

// --- On battery -----------------------------------------------------------

fn on_battery() -> Check {
    if cfg!(target_os = "macos") {
        match std::process::Command::new("pmset")
            .args(["-g", "batt"])
            .output()
        {
            Ok(out) if out.status.success() => {
                on_battery_from_pmset(&String::from_utf8_lossy(&out.stdout))
            }
            _ => check(
                "power source",
                true,
                "not checked: `pmset -g batt` unavailable",
            ),
        }
    } else if cfg!(target_os = "linux") {
        on_battery_linux_at(Path::new("/sys/class/power_supply"))
    } else {
        check("power source", true, "not checked on this platform")
    }
}

fn on_battery_from_pmset(s: &str) -> Check {
    if s.contains("Battery Power") {
        check(
            "power source",
            false,
            "running on battery power — a thermally throttled laptop on battery produces noise \
             dressed as data",
        )
    } else if s.contains("AC Power") {
        check("power source", true, "on AC power")
    } else {
        check(
            "power source",
            true,
            "not checked: could not parse `pmset -g batt`",
        )
    }
}

/// Injectable by base directory, mirroring the Linux checks above: real
/// production code always passes `/sys/class/power_supply`.
fn on_battery_linux_at(base: &Path) -> Check {
    // A handful of common AC-adapter names; there is no single standard one.
    for name in ["AC", "AC0", "ADP1", "ACAD"] {
        let path = base.join(name).join("online");
        if let Ok(s) = std::fs::read_to_string(&path) {
            let ok = s.trim() == "1";
            return check(
                "power source",
                ok,
                if ok {
                    "on AC power".to_string()
                } else {
                    "running on battery power — a thermally throttled laptop on battery produces \
                     noise dressed as data"
                        .to_string()
                },
            );
        }
    }
    check(
        "power source",
        true,
        "not checked: no AC power_supply found under /sys/class/power_supply (desktop, or an \
         unrecognised name)",
    )
}

// --- Rust-specific: CARGO_INCREMENTAL ---------------------------------------

fn cargo_incremental_var(v: Option<String>) -> Check {
    match v {
        Some(v) if !v.is_empty() && v != "0" => check(
            "CARGO_INCREMENTAL",
            false,
            format!(
                "CARGO_INCREMENTAL={v} is set — incremental artifacts make round timings jumpy; \
                 unset it before measuring"
            ),
        ),
        _ => check("CARGO_INCREMENTAL", true, "not set"),
    }
}

fn cargo_incremental() -> Check {
    cargo_incremental_var(std::env::var("CARGO_INCREMENTAL").ok())
}

// --- Rust-specific: [profile.bench] opt-level override ----------------------

fn bench_profile_opt_level_in(text: &str) -> Check {
    let value: toml::Value = match toml::from_str(text) {
        Ok(v) => v,
        Err(_) => {
            return check(
                "[profile.bench]",
                true,
                "not checked: Cargo.toml did not parse",
            );
        }
    };
    let opt_level = value
        .get("profile")
        .and_then(|p| p.get("bench"))
        .and_then(|b| b.get("opt-level"));
    match opt_level {
        Some(v) => check(
            "[profile.bench]",
            false,
            format!(
                "opt-level = {v} overrides criterion's own default — codegen differs from what a \
                 reader comparing numbers across runs or repositories would assume"
            ),
        ),
        None => check("[profile.bench]", true, "opt-level not overridden"),
    }
}

fn bench_profile_opt_level(root: &Path) -> Check {
    match std::fs::read_to_string(root.join("Cargo.toml")) {
        Ok(text) => bench_profile_opt_level_in(&text),
        Err(_) => check(
            "[profile.bench]",
            true,
            "not checked: no Cargo.toml at the repository root",
        ),
    }
}

// --- Rust-specific: nightly default toolchain -------------------------------

fn nightly_default_toolchain_from(version_line: &str) -> Check {
    let nightly = version_line.contains("nightly");
    check(
        "default toolchain",
        !nightly,
        if nightly {
            format!(
                "{} — nightly codegen differs from the stable a reader would assume",
                version_line.trim()
            )
        } else {
            version_line.trim().to_string()
        },
    )
}

fn nightly_default_toolchain(root: &Path) -> Check {
    match std::process::Command::new("rustc")
        .arg("--version")
        .current_dir(root)
        .output()
    {
        Ok(out) if out.status.success() => {
            nightly_default_toolchain_from(&String::from_utf8_lossy(&out.stdout))
        }
        _ => check(
            "default toolchain",
            true,
            "not checked: `rustc --version` unavailable",
        ),
    }
}

// --- Rust-specific: dependency cooldown ([registry] global-min-publish-age) --

/// Cargo config files that could hold `[registry] global-min-publish-age`,
/// most specific first: the repository's own `.cargo`, then `CARGO_HOME`,
/// then the default `$HOME/.cargo`. Cargo itself looks for `config.toml`
/// before the older `config` in each directory, so this mirrors that.
fn cargo_config_candidates(
    root: &Path,
    cargo_home: Option<&Path>,
    home: Option<&Path>,
) -> Vec<PathBuf> {
    let mut dirs = vec![root.join(".cargo")];
    if let Some(h) = cargo_home {
        dirs.push(h.to_path_buf());
    }
    if let Some(h) = home {
        dirs.push(h.join(".cargo"));
    }
    let mut out: Vec<PathBuf> = Vec::new();
    for dir in dirs {
        for name in ["config.toml", "config"] {
            let p = dir.join(name);
            // `CARGO_HOME` often *is* `$HOME/.cargo`; do not read it twice.
            if !out.contains(&p) {
                out.push(p);
            }
        }
    }
    out
}

/// Whether Cargo is configured to refuse crates newer than a cooldown window
/// (RFC 3923). Injectable by directory so tests never touch the real
/// `$CARGO_HOME` or `$HOME`.
fn dependency_cooldown_at(root: &Path, cargo_home: Option<&Path>, home: Option<&Path>) -> Check {
    const NAME: &str = "dependency cooldown";
    for path in cargo_config_candidates(root, cargo_home, home) {
        let text = match std::fs::read_to_string(&path) {
            Ok(t) => t,
            // Missing files are the normal case and simply mean "keep
            // looking"; only a file that exists but cannot be read is a
            // finding.
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => continue,
            Err(e) => {
                return check(
                    NAME,
                    false,
                    format!("could not read {}: {e}", path.display()),
                );
            }
        };
        let value: toml::Value = match toml::from_str(&text) {
            Ok(v) => v,
            Err(e) => {
                return check(
                    NAME,
                    false,
                    format!("{} is not valid TOML: {e}", path.display()),
                );
            }
        };
        let Some(v) = value
            .get("registry")
            .and_then(|r| r.get("global-min-publish-age"))
        else {
            // A readable config that does not set the key: keep looking in
            // the less specific locations.
            continue;
        };
        let value = match v.as_str() {
            Some(s) => s.to_string(),
            None => v.to_string(),
        };
        return check(NAME, true, format!("{value} — set in {}", path.display()));
    }
    check(
        NAME,
        false,
        "not set — consider [registry] global-min-publish-age = \"3 days\" in \
         .cargo/config.toml (malicious releases are usually caught within days, RFC 3923)",
    )
}

fn dependency_cooldown(root: &Path) -> Check {
    let cargo_home = std::env::var_os("CARGO_HOME").map(PathBuf::from);
    let home = dirs::home_dir();
    dependency_cooldown_at(root, cargo_home.as_deref(), home.as_deref())
}

// --- Rust-specific: how fresh the compiler is -------------------------------

/// Days from 1970-01-01 to `y-m-d` in the proleptic Gregorian calendar,
/// negative before the epoch. Howard Hinnant's `days_from_civil`, the standard
/// all-integer civil-from-days algorithm.
fn days_since_epoch(y: i64, m: i64, d: i64) -> i64 {
    let y = if m <= 2 { y - 1 } else { y };
    // Shift negative years into the same 400-year era arithmetic as positive
    // ones instead of relying on Rust's truncating division.
    let era = if y >= 0 { y } else { y - 399 } / 400;
    let yoe = y - era * 400; // [0, 399]
    let mp = (m + 9) % 12; // [0, 11], March-based
    let doy = (153 * mp + 2) / 5 + d - 1; // [0, 365]
    let doe = yoe * 365 + yoe / 4 - yoe / 100 + doy; // [0, 146096]
    era * 146_097 + doe - 719_468
}

/// Parses a strict `YYYY-MM-DD` into `(year, month, day)`.
fn parse_ymd(s: &str) -> Option<(i64, i64, i64)> {
    let mut parts = s.split('-');
    let y = parts.next()?.parse().ok()?;
    let m = parts.next()?.parse().ok()?;
    let d = parts.next()?.parse().ok()?;
    if parts.next().is_some() || !(1..=12).contains(&m) || !(1..=31).contains(&d) {
        return None;
    }
    Some((y, m, d))
}

/// Reads one `key: value` line out of `rustc -Vv`, e.g. `release: 1.100.0`.
fn vv_field<'a>(vv: &'a str, key: &str) -> Option<&'a str> {
    vv.lines()
        .find_map(|line| line.strip_prefix(key))
        .map(str::trim)
        .filter(|v| !v.is_empty())
}

/// How many days old a release must be before its codegen is considered
/// settled.
const TOOLCHAIN_AGE_WARN_DAYS: i64 = 7;

/// Turns `rustc -Vv` into a finding. Pure: `today_days` is injected so tests
/// neither run `rustc` nor depend on the current date.
fn toolchain_age_from(vv: &str, today_days: i64) -> Check {
    const NAME: &str = "toolchain age";
    let Some(release) = vv_field(vv, "release:") else {
        return check(NAME, true, "not checked: no `release:` line in `rustc -Vv`");
    };
    let Some(date) = vv_field(vv, "commit-date:") else {
        return check(
            NAME,
            true,
            "not checked: no `commit-date:` line in `rustc -Vv`",
        );
    };
    let Some((y, m, d)) = parse_ymd(date) else {
        return check(
            NAME,
            true,
            format!("not checked: `commit-date: {date}` is not YYYY-MM-DD"),
        );
    };
    let age = today_days - days_since_epoch(y, m, d);
    if age < 0 {
        return check(
            NAME,
            true,
            format!(
                "not checked: commit-date {date} is {} day(s) in the future — the system clock or \
                 the toolchain metadata is off",
                -age
            ),
        );
    }
    let channel = if release.contains("nightly") {
        Some("nightly")
    } else if release.contains("beta") {
        Some("beta")
    } else {
        None
    };
    // `-Z` flags only exist on nightly, so the lever is offered for nightly
    // alone; beta gets a note that its codegen is not the last stable's.
    let note = match channel {
        Some("nightly") => {
            "; nightly channel: -Zunstable-options --jobs-frontend=N is a build-time lever for \
             faster builds"
        }
        Some("beta") => "; beta channel: codegen can differ from the last stable release",
        _ => "",
    };
    let day_word = if age == 1 { "day" } else { "days" };
    if age < TOOLCHAIN_AGE_WARN_DAYS {
        check(
            NAME,
            false,
            format!(
                "rustc {release} released {age} {day_word} ago — codegen fixes are still landing; \
                 timings can change with a fresh compiler{note}"
            ),
        )
    } else {
        check(
            NAME,
            true,
            format!("rustc {release}, released {age} {day_word} ago{note}"),
        )
    }
}

fn today_days() -> i64 {
    match std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH) {
        Ok(d) => (d.as_secs() / 86_400) as i64,
        Err(_) => 0,
    }
}

fn toolchain_age() -> Check {
    match std::process::Command::new("rustc").arg("-Vv").output() {
        Ok(out) if out.status.success() => {
            toolchain_age_from(&String::from_utf8_lossy(&out.stdout), today_days())
        }
        Ok(_) => check("toolchain age", true, "not checked: `rustc -Vv` failed"),
        Err(e) => check(
            "toolchain age",
            true,
            format!("not checked: `rustc -Vv` unavailable ({e})"),
        ),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_performance_governor_is_ok() {
        let f = tempfile::NamedTempFile::new().unwrap();
        std::fs::write(f.path(), "performance\n").unwrap();
        let c = cpu_frequency_scaling_at(f.path());
        assert!(c.ok, "{}", c.detail);
    }

    #[test]
    fn a_powersave_governor_is_flagged() {
        let f = tempfile::NamedTempFile::new().unwrap();
        std::fs::write(f.path(), "powersave\n").unwrap();
        let c = cpu_frequency_scaling_at(f.path());
        assert!(!c.ok);
        assert!(c.detail.contains("powersave"), "{}", c.detail);
    }

    #[test]
    fn a_missing_governor_file_is_not_a_finding() {
        let d = tempfile::tempdir().unwrap();
        let c = cpu_frequency_scaling_at(&d.path().join("does-not-exist"));
        assert!(
            c.ok,
            "a VM/container without cpufreq must not read as a problem"
        );
    }

    #[test]
    fn zero_throttle_events_is_ok() {
        let f = tempfile::NamedTempFile::new().unwrap();
        std::fs::write(f.path(), "0\n").unwrap();
        assert!(thermal_throttle_risk_at(f.path()).ok);
    }

    #[test]
    fn nonzero_throttle_events_is_flagged() {
        let f = tempfile::NamedTempFile::new().unwrap();
        std::fs::write(f.path(), "42\n").unwrap();
        let c = thermal_throttle_risk_at(f.path());
        assert!(!c.ok);
        assert!(c.detail.contains("42"));
    }

    #[test]
    fn pmset_at_full_speed_is_ok() {
        let c = thermal_throttle_from_pmset("CPU_Speed_Limit\t= 100\n");
        assert!(c.ok, "{}", c.detail);
    }

    #[test]
    fn pmset_below_full_speed_is_flagged() {
        let c = thermal_throttle_from_pmset("CPU_Speed_Limit\t= 75\n");
        assert!(!c.ok);
        assert!(c.detail.contains("75"));
    }

    #[test]
    #[cfg(unix)]
    fn a_quiet_machine_is_under_the_load_threshold() {
        assert!(load_average_finding(1.0, 8.0).ok);
    }

    #[test]
    #[cfg(unix)]
    fn a_busy_machine_exceeds_the_load_threshold() {
        let c = load_average_finding(7.0, 8.0);
        assert!(!c.ok, "{}", c.detail);
    }

    #[test]
    #[cfg(unix)]
    fn plenty_of_disk_is_ok() {
        assert!(disk_space_finding(10 * 1024 * 1024 * 1024).ok);
    }

    #[test]
    #[cfg(unix)]
    fn low_disk_is_flagged() {
        let c = disk_space_finding(1024 * 1024 * 1024);
        assert!(!c.ok, "{}", c.detail);
    }

    #[test]
    fn pmset_on_battery_power_is_flagged() {
        assert!(!on_battery_from_pmset("Now drawing from 'Battery Power'\n").ok);
    }

    #[test]
    fn pmset_on_ac_power_is_ok() {
        assert!(on_battery_from_pmset("Now drawing from 'AC Power'\n").ok);
    }

    #[test]
    fn linux_ac_online_is_ok() {
        let d = tempfile::tempdir().unwrap();
        std::fs::create_dir_all(d.path().join("AC")).unwrap();
        std::fs::write(d.path().join("AC/online"), "1\n").unwrap();
        assert!(on_battery_linux_at(d.path()).ok);
    }

    #[test]
    fn linux_ac_offline_is_flagged() {
        let d = tempfile::tempdir().unwrap();
        std::fs::create_dir_all(d.path().join("AC")).unwrap();
        std::fs::write(d.path().join("AC/online"), "0\n").unwrap();
        assert!(!on_battery_linux_at(d.path()).ok);
    }

    #[test]
    fn a_desktop_with_no_power_supply_entries_is_not_a_finding() {
        let d = tempfile::tempdir().unwrap();
        assert!(on_battery_linux_at(d.path()).ok);
    }

    #[test]
    fn cargo_incremental_unset_is_ok() {
        assert!(cargo_incremental_var(None).ok);
    }

    #[test]
    fn cargo_incremental_zero_is_ok() {
        assert!(cargo_incremental_var(Some("0".to_string())).ok);
    }

    #[test]
    fn cargo_incremental_set_is_flagged() {
        let c = cargo_incremental_var(Some("1".to_string()));
        assert!(!c.ok);
        assert!(c.detail.contains("CARGO_INCREMENTAL=1"));
    }

    #[test]
    fn no_profile_bench_override_is_ok() {
        assert!(bench_profile_opt_level_in("[package]\nname = \"x\"\n").ok);
    }

    #[test]
    fn a_profile_bench_opt_level_override_is_flagged() {
        let c = bench_profile_opt_level_in("[profile.bench]\nopt-level = 0\n");
        assert!(!c.ok);
        assert!(c.detail.contains("opt-level"));
    }

    #[test]
    fn unparseable_cargo_toml_is_not_a_finding() {
        assert!(bench_profile_opt_level_in("not valid toml [[[").ok);
    }

    #[test]
    fn a_stable_toolchain_is_ok() {
        assert!(nightly_default_toolchain_from("rustc 1.85.0 (abcdef 2026-01-01)\n").ok);
    }

    #[test]
    fn a_nightly_toolchain_is_flagged() {
        let c = nightly_default_toolchain_from("rustc 1.90.0-nightly (abcdef 2026-01-01)\n");
        assert!(!c.ok);
        assert!(c.detail.contains("nightly"));
    }

    // --- dependency cooldown ------------------------------------------------

    #[test]
    fn a_cooldown_in_the_repository_config_is_ok() {
        let d = tempfile::tempdir().unwrap();
        std::fs::create_dir_all(d.path().join(".cargo")).unwrap();
        std::fs::write(
            d.path().join(".cargo/config.toml"),
            "[registry]\nglobal-min-publish-age = \"3 days\"\n",
        )
        .unwrap();
        let c = dependency_cooldown_at(d.path(), None, None);
        assert!(c.ok, "{}", c.detail);
        assert!(c.detail.contains("3 days"), "{}", c.detail);
        assert!(c.detail.contains("config.toml"), "{}", c.detail);
    }

    #[test]
    fn a_missing_cooldown_is_flagged_with_a_suggestion() {
        let d = tempfile::tempdir().unwrap();
        let c = dependency_cooldown_at(d.path(), None, None);
        assert!(!c.ok);
        assert!(c.detail.contains("global-min-publish-age"), "{}", c.detail);
        assert!(c.detail.contains("3 days"), "{}", c.detail);
    }

    #[test]
    fn a_cooldown_in_cargo_home_is_found() {
        let root = tempfile::tempdir().unwrap();
        let cargo_home = tempfile::tempdir().unwrap();
        std::fs::write(
            cargo_home.path().join("config.toml"),
            "[registry]\nglobal-min-publish-age = \"7 days\"\n",
        )
        .unwrap();
        let c = dependency_cooldown_at(root.path(), Some(cargo_home.path()), None);
        assert!(c.ok, "{}", c.detail);
        assert!(c.detail.contains("7 days"), "{}", c.detail);
        assert!(
            c.detail.contains(&cargo_home.path().display().to_string()),
            "{}",
            c.detail
        );
    }

    #[test]
    fn a_cooldown_in_the_home_cargo_config_is_found() {
        let root = tempfile::tempdir().unwrap();
        let home = tempfile::tempdir().unwrap();
        std::fs::create_dir_all(home.path().join(".cargo")).unwrap();
        std::fs::write(
            home.path().join(".cargo/config.toml"),
            "[registry]\nglobal-min-publish-age = \"2 days\"\n",
        )
        .unwrap();
        let c = dependency_cooldown_at(root.path(), None, Some(home.path()));
        assert!(c.ok, "{}", c.detail);
        assert!(c.detail.contains("2 days"), "{}", c.detail);
    }

    #[test]
    fn a_dotless_cargo_config_file_is_accepted() {
        let d = tempfile::tempdir().unwrap();
        std::fs::create_dir_all(d.path().join(".cargo")).unwrap();
        std::fs::write(
            d.path().join(".cargo/config"),
            "[registry]\nglobal-min-publish-age = \"4 days\"\n",
        )
        .unwrap();
        assert!(dependency_cooldown_at(d.path(), None, None).ok);
    }

    #[test]
    fn the_repository_config_wins_over_cargo_home() {
        let root = tempfile::tempdir().unwrap();
        let cargo_home = tempfile::tempdir().unwrap();
        std::fs::create_dir_all(root.path().join(".cargo")).unwrap();
        std::fs::write(
            root.path().join(".cargo/config.toml"),
            "[registry]\nglobal-min-publish-age = \"3 days\"\n",
        )
        .unwrap();
        std::fs::write(
            cargo_home.path().join("config.toml"),
            "[registry]\nglobal-min-publish-age = \"30 days\"\n",
        )
        .unwrap();
        let c = dependency_cooldown_at(root.path(), Some(cargo_home.path()), None);
        assert!(c.ok, "{}", c.detail);
        assert!(c.detail.contains("3 days"), "{}", c.detail);
        assert!(!c.detail.contains("30 days"), "{}", c.detail);
    }

    #[test]
    fn a_malformed_cargo_config_is_flagged_naming_the_path() {
        let d = tempfile::tempdir().unwrap();
        std::fs::create_dir_all(d.path().join(".cargo")).unwrap();
        let path = d.path().join(".cargo/config.toml");
        std::fs::write(&path, "not valid toml [[[").unwrap();
        let c = dependency_cooldown_at(d.path(), None, None);
        assert!(!c.ok, "{}", c.detail);
        assert!(
            c.detail.contains(&path.display().to_string()),
            "{}",
            c.detail
        );
    }

    #[test]
    fn an_unreadable_cargo_config_is_flagged_naming_the_path() {
        let d = tempfile::tempdir().unwrap();
        // A directory where a config file should be: `read_to_string` fails.
        std::fs::create_dir_all(d.path().join(".cargo/config.toml")).unwrap();
        let c = dependency_cooldown_at(d.path(), None, None);
        assert!(!c.ok, "{}", c.detail);
        assert!(c.detail.contains("config.toml"), "{}", c.detail);
    }

    // --- toolchain age ------------------------------------------------------

    #[test]
    fn days_since_epoch_matches_well_known_dates() {
        assert_eq!(days_since_epoch(1970, 1, 1), 0);
        assert_eq!(days_since_epoch(2000, 1, 1), 10957);
        assert_eq!(days_since_epoch(2026, 11, 17), 20774);
        assert_eq!(days_since_epoch(2026, 11, 18), 20775);
        // Leap day, both a leap and a non-leap February.
        assert_eq!(
            days_since_epoch(2024, 3, 1),
            days_since_epoch(2024, 2, 29) + 1
        );
        assert_eq!(
            days_since_epoch(2023, 3, 1),
            days_since_epoch(2023, 2, 28) + 1
        );
    }

    const FRESH_VV: &str = "rustc 1.100.0 (5e3d9a4c3 2026-11-15)\n\
binary: rustc\n\
commit-hash: 5e3d9a4c3f0a0d1e2f3a4b5c6d7e8f9012345678\n\
commit-date: 2026-11-15\n\
host: x86_64-unknown-linux-gnu\n\
release: 1.100.0\n\
LLVM version: 21.1.2\n";

    #[test]
    fn a_toolchain_released_days_ago_is_flagged() {
        let today = days_since_epoch(2026, 11, 17);
        let c = toolchain_age_from(FRESH_VV, today);
        assert!(!c.ok, "{}", c.detail);
        assert!(c.detail.contains("2 days ago"), "{}", c.detail);
        assert!(c.detail.contains("timings can change"), "{}", c.detail);
    }

    #[test]
    fn a_toolchain_released_today_is_flagged() {
        let today = days_since_epoch(2026, 11, 15);
        let c = toolchain_age_from(FRESH_VV, today);
        assert!(!c.ok, "{}", c.detail);
        assert!(c.detail.contains("0 days ago"), "{}", c.detail);
    }

    #[test]
    fn a_toolchain_seven_days_old_is_no_longer_flagged() {
        let today = days_since_epoch(2026, 11, 22);
        let c = toolchain_age_from(FRESH_VV, today);
        assert!(c.ok, "{}", c.detail);
        assert!(c.detail.contains("7 days ago"), "{}", c.detail);
    }

    #[test]
    fn an_older_stable_toolchain_is_ok_and_names_the_version() {
        let vv = FRESH_VV.replace("2026-11-15", "2026-09-01");
        let today = days_since_epoch(2026, 11, 17);
        let c = toolchain_age_from(&vv, today);
        assert!(c.ok, "{}", c.detail);
        assert!(c.detail.contains("1.100.0"), "{}", c.detail);
        assert!(!c.detail.contains("--jobs-frontend"), "{}", c.detail);
    }

    #[test]
    fn a_nightly_toolchain_mentions_the_frontend_jobs_lever() {
        let vv = FRESH_VV.replace("release: 1.100.0", "release: 1.93.0-nightly");
        let today = days_since_epoch(2026, 11, 17);
        let c = toolchain_age_from(&vv, today);
        assert!(!c.ok, "{}", c.detail);
        assert!(c.detail.contains("-Zunstable-options"), "{}", c.detail);
        assert!(c.detail.contains("--jobs-frontend=N"), "{}", c.detail);
        assert!(c.detail.contains("nightly"), "{}", c.detail);
    }

    #[test]
    fn an_old_nightly_still_mentions_the_frontend_jobs_lever() {
        let vv = FRESH_VV
            .replace("release: 1.100.0", "release: 1.93.0-nightly")
            .replace("2026-11-15", "2026-09-01");
        let today = days_since_epoch(2026, 11, 17);
        let c = toolchain_age_from(&vv, today);
        assert!(c.ok, "{}", c.detail);
        assert!(c.detail.contains("--jobs-frontend=N"), "{}", c.detail);
    }

    #[test]
    fn a_beta_toolchain_is_named_as_beta() {
        let vv = FRESH_VV.replace("release: 1.100.0", "release: 1.101.0-beta.3");
        let today = days_since_epoch(2026, 11, 17);
        let c = toolchain_age_from(&vv, today);
        assert!(c.detail.contains("beta"), "{}", c.detail);
    }

    #[test]
    fn a_vv_without_a_commit_date_is_not_a_finding() {
        let c = toolchain_age_from("rustc 1.100.0 (abc 2026-11-15)\nrelease: 1.100.0\n", 20774);
        assert!(c.ok, "{}", c.detail);
        assert!(c.detail.starts_with("not checked:"), "{}", c.detail);
    }

    #[test]
    fn a_vv_without_a_release_line_is_not_a_finding() {
        let c = toolchain_age_from("commit-date: 2026-11-15\n", 20774);
        assert!(c.ok, "{}", c.detail);
        assert!(c.detail.starts_with("not checked:"), "{}", c.detail);
    }

    #[test]
    fn an_unparsable_commit_date_is_not_a_finding() {
        let vv = FRESH_VV.replace("commit-date: 2026-11-15", "commit-date: yesterday");
        let c = toolchain_age_from(&vv, 20774);
        assert!(c.ok, "{}", c.detail);
        assert!(c.detail.starts_with("not checked:"), "{}", c.detail);
    }

    #[test]
    fn empty_rustc_output_is_not_a_finding() {
        let c = toolchain_age_from("", 20774);
        assert!(c.ok, "{}", c.detail);
        assert!(c.detail.starts_with("not checked:"), "{}", c.detail);
    }

    #[test]
    fn a_commit_date_in_the_future_is_not_a_finding() {
        let today = days_since_epoch(2026, 11, 1);
        let c = toolchain_age_from(FRESH_VV, today);
        assert!(c.ok, "{}", c.detail);
        assert!(c.detail.contains("future"), "{}", c.detail);
    }

    #[test]
    fn run_always_returns_every_check_and_never_panics() {
        let d = tempfile::tempdir().unwrap();
        let checks = run(d.path());
        assert!(!checks.is_empty());
        for c in &checks {
            assert!(!c.name.is_empty());
            assert!(!c.detail.is_empty());
        }
    }
}
