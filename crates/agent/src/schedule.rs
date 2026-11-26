//! Scheduled tasks: cron jobs (system and, when readable, user crontabs), systemd timers and Windows Task Scheduler.
//!
//! The list is read-only. Cron files are parsed here (including a small cron-expression engine for
//! "next runs" and a plain-English summary); timers and Windows tasks come from `services.rs`.

use anyhow::Result;
use serde_json::{json, Value};

use crate::services::Services;

// ---------------------------------------------------------------- cron expressions

#[derive(Debug, Clone, PartialEq)]
pub struct CronExpr {
    min: Vec<u32>,
    hour: Vec<u32>,
    dom: Vec<u32>,
    month: Vec<u32>,
    dow: Vec<u32>,
    dom_star: bool,
    dow_star: bool,
    pub reboot: bool,
}

fn name_to_num(s: &str, names: &[&str], base: u32) -> Option<u32> {
    let l = s.to_ascii_lowercase();
    names.iter().position(|n| *n == l).map(|i| i as u32 + base)
}

fn parse_field(f: &str, lo: u32, hi: u32, names: &[&str], names_base: u32) -> Option<(Vec<u32>, bool)> {
    let num = |s: &str| -> Option<u32> { s.parse().ok().or_else(|| name_to_num(s, names, names_base)) };
    let mut out = Vec::new();
    let star = f == "*" || f == "?";
    for part in f.split(',') {
        let (range, step) = match part.split_once('/') {
            Some((r, s)) => (r, s.parse::<u32>().ok().filter(|s| *s > 0)?),
            None => (part, 1),
        };
        let (a, b) = if range == "*" || range == "?" {
            (lo, hi)
        } else if let Some((a, b)) = range.split_once('-') {
            (num(a)?, num(b)?)
        } else {
            let v = num(range)?;
            (v, if part.contains('/') { hi } else { v })
        };
        if a < lo || b > hi.max(7) || a > b {
            return None;
        }
        let mut v = a;
        while v <= b {
            out.push(v);
            v += step;
        }
    }
    out.sort_unstable();
    out.dedup();
    Some((out, star))
}

impl CronExpr {
    /// Five fields or an `@alias`.
    pub fn parse(spec: &str) -> Option<Self> {
        let spec = spec.trim();
        let expanded = match spec {
            "@reboot" => {
                return Some(Self { min: vec![], hour: vec![], dom: vec![], month: vec![], dow: vec![], dom_star: true, dow_star: true, reboot: true });
            }
            "@yearly" | "@annually" => "0 0 1 1 *",
            "@monthly" => "0 0 1 * *",
            "@weekly" => "0 0 * * 0",
            "@daily" | "@midnight" => "0 0 * * *",
            "@hourly" => "0 * * * *",
            other => other,
        };
        let f: Vec<&str> = expanded.split_whitespace().collect();
        if f.len() != 5 {
            return None;
        }
        const MONTHS: [&str; 12] = ["jan", "feb", "mar", "apr", "may", "jun", "jul", "aug", "sep", "oct", "nov", "dec"];
        const DAYS: [&str; 7] = ["sun", "mon", "tue", "wed", "thu", "fri", "sat"];
        let (min, _) = parse_field(f[0], 0, 59, &[], 0)?;
        let (hour, _) = parse_field(f[1], 0, 23, &[], 0)?;
        let (dom, dom_star) = parse_field(f[2], 1, 31, &[], 1)?;
        let (month, _) = parse_field(f[3], 1, 12, &MONTHS, 1)?;
        let (mut dow, dow_star) = parse_field(f[4], 0, 7, &DAYS, 0)?;
        for d in dow.iter_mut() {
            if *d == 7 {
                *d = 0;
            }
        }
        dow.sort_unstable();
        dow.dedup();
        if min.iter().any(|v| *v > 59) || hour.iter().any(|v| *v > 23) || dom.iter().any(|v| *v > 31) || month.iter().any(|v| *v > 12) {
            return None;
        }
        Some(Self { min, hour, dom, month, dow, dom_star, dow_star, reboot: false })
    }

    fn day_matches(&self, month: u32, dom: u32, dow: u32) -> bool {
        if !self.month.contains(&month) {
            return false;
        }
        let d = self.dom.contains(&dom);
        let w = self.dow.contains(&dow);
        match (self.dom_star, self.dow_star) {
            (true, true) => true,
            (false, true) => d,
            (true, false) => w,
            (false, false) => d || w, // classic cron: either matches
        }
    }

    /// The next `n` run times after `after_local` (seconds since the epoch on a naive local clock).
    pub fn next_runs(&self, after_local: i64, n: usize) -> Vec<i64> {
        let mut out = Vec::new();
        if self.reboot {
            return out;
        }
        let start_day = after_local.div_euclid(86_400);
        for day in start_day..start_day + 800 {
            let (_, m, d) = civil_from_days(day);
            let dow = (day + 4).rem_euclid(7) as u32; // 1970-01-01 was a Thursday
            if !self.day_matches(m, d, dow) {
                continue;
            }
            for h in &self.hour {
                for mi in &self.min {
                    let t = day * 86_400 + (*h as i64) * 3600 + (*mi as i64) * 60;
                    if t > after_local {
                        out.push(t);
                        if out.len() >= n {
                            return out;
                        }
                    }
                }
            }
        }
        out
    }

    /// A short plain-English summary; falls back to an empty string for unusual expressions.
    pub fn describe(&self) -> String {
        if self.reboot {
            return "At system start".into();
        }
        let all = |v: &Vec<u32>, n: u32| v.len() as u32 == n;
        let every_day = self.dom_star && self.dow_star && all(&self.month, 12);
        let hhmm = |h: u32, m: u32| format!("{h:02}:{m:02}");
        let step = |v: &Vec<u32>, max: u32| -> Option<u32> {
            if v.len() < 2 || v[0] != 0 {
                return None;
            }
            let s = v[1] - v[0];
            (v.windows(2).all(|w| w[1] - w[0] == s) && v.last().map(|l| l + s >= max).unwrap_or(false)).then_some(s)
        };
        if every_day && all(&self.hour, 24) {
            if all(&self.min, 60) {
                return "Every minute".into();
            }
            if let Some(s) = step(&self.min, 60) {
                return format!("Every {s} minutes");
            }
            if self.min.len() == 1 {
                return if self.min[0] == 0 { "Every hour".into() } else { format!("Every hour at minute {}", self.min[0]) };
            }
        }
        if self.min.len() == 1 && every_day {
            if self.hour.len() == 1 {
                return format!("Every day at {}", hhmm(self.hour[0], self.min[0]));
            }
            if let Some(s) = step(&self.hour, 24) {
                return format!("Every {s} hours at minute {}", self.min[0]);
            }
        }
        if self.min.len() == 1 && self.hour.len() == 1 && self.dom_star && !self.dow_star && all(&self.month, 12) {
            const D: [&str; 7] = ["Sunday", "Monday", "Tuesday", "Wednesday", "Thursday", "Friday", "Saturday"];
            let days: Vec<&str> = self.dow.iter().map(|d| D[*d as usize % 7]).collect();
            return format!("Every {} at {}", days.join(", "), hhmm(self.hour[0], self.min[0]));
        }
        if self.min.len() == 1 && self.hour.len() == 1 && !self.dom_star && self.dow_star && all(&self.month, 12) {
            let days: Vec<String> = self.dom.iter().map(|d| d.to_string()).collect();
            return format!("Day {} of every month at {}", days.join(", "), hhmm(self.hour[0], self.min[0]));
        }
        String::new()
    }
}

/// Days since 1970-01-01 → (year, month, day).
fn civil_from_days(z: i64) -> (i64, u32, u32) {
    let z = z + 719_468;
    let era = z.div_euclid(146_097);
    let doe = z.rem_euclid(146_097);
    let yoe = (doe - doe / 1460 + doe / 36_524 - doe / 146_096) / 365;
    let y = yoe + era * 400;
    let doy = doe - (365 * yoe + yoe / 4 - yoe / 100);
    let mp = (5 * doy + 2) / 153;
    let d = (doy - (153 * mp + 2) / 5 + 1) as u32;
    let m = if mp < 10 { mp + 3 } else { mp - 9 } as u32;
    (if m <= 2 { y + 1 } else { y }, m, d)
}

// ---------------------------------------------------------------- crontab files

#[derive(Debug, Clone, PartialEq)]
pub struct CronEntry {
    pub schedule: String,
    pub user: String,
    pub command: String,
    pub source: String,
    pub line: usize,
}

fn is_env_line(l: &str) -> bool {
    match l.split_once('=') {
        Some((k, _)) => !k.is_empty() && !k.trim_end().contains(char::is_whitespace) && k.trim_end().chars().all(|c| c.is_ascii_alphanumeric() || c == '_'),
        None => false,
    }
}

/// Split off `n` whitespace-separated fields; the rest of the line (untouched) is the command.
fn take_fields(line: &str, n: usize) -> Option<(Vec<&str>, &str)> {
    let mut rest = line.trim_start();
    let mut fields = Vec::new();
    for _ in 0..n {
        let end = rest.find(char::is_whitespace)?;
        fields.push(&rest[..end]);
        rest = rest[end..].trim_start();
    }
    (!rest.is_empty()).then_some((fields, rest))
}

/// `system`: the format of /etc/crontab and /etc/cron.d (a user column); otherwise a user crontab owned by `owner`.
pub fn parse_crontab(text: &str, source: &str, system: bool, owner: &str) -> Vec<CronEntry> {
    let mut out = Vec::new();
    for (i, raw) in text.lines().enumerate() {
        let l = raw.trim();
        if l.is_empty() || l.starts_with('#') || is_env_line(l) {
            continue;
        }
        let special = l.starts_with('@');
        let n = if special { 1 } else { 5 };
        let extra = if system { 1 } else { 0 };
        let Some((fields, cmd)) = take_fields(l, n + extra) else { continue };
        let schedule = fields[..n].join(" ");
        if CronExpr::parse(&schedule).is_none() {
            continue;
        }
        let user = if system { fields[n].to_string() } else { owner.to_string() };
        out.push(CronEntry { schedule, user, command: cmd.trim().to_string(), source: source.to_string(), line: i + 1 });
    }
    out
}

#[cfg(unix)]
mod unix_impl {
    use super::*;
    use crate::util::host_path;
    use std::collections::HashMap;
    use std::path::Path;
    use std::process::Stdio;
    use std::time::Duration;

    pub fn cron_unit(root: Option<&str>) -> &'static str {
        for (unit, dirs) in [
            ("cron.service", ["/lib/systemd/system", "/usr/lib/systemd/system", "/etc/systemd/system"]),
            ("crond.service", ["/lib/systemd/system", "/usr/lib/systemd/system", "/etc/systemd/system"]),
        ] {
            if dirs.iter().any(|d| host_path(root, &format!("{d}/{unit}")).exists()) {
                return unit;
            }
        }
        "cron.service"
    }

    async fn local_offset_secs() -> i64 {
        let out = tokio::process::Command::new("date").arg("+%z").output().await.ok();
        let s = out.map(|o| String::from_utf8_lossy(&o.stdout).trim().to_string()).unwrap_or_default();
        if s.len() == 5 {
            let sign = if s.starts_with('-') { -1 } else { 1 };
            let h: i64 = s[1..3].parse().unwrap_or(0);
            let m: i64 = s[3..5].parse().unwrap_or(0);
            return sign * (h * 3600 + m * 60);
        }
        0
    }

    /// Most recent run per (user, command), read from the journal's cron lines (best effort).
    async fn last_runs(root: Option<&str>) -> HashMap<(String, String), i64> {
        let jc = std::env::var("MONITORLAB_JOURNALCTL").unwrap_or_else(|_| "journalctl".into());
        let mut cmd = tokio::process::Command::new(jc);
        cmd.args(["-o", "json", "--no-pager", "--since", "-14 days", "-n", "30000", "-g", r"CMD \(", "--output-fields=MESSAGE,SYSLOG_IDENTIFIER"]);
        for t in ["CRON", "CROND", "cron", "crond"] {
            cmd.args(["-t", t]);
        }
        if root.map(|r| !r.is_empty() && r != "/").unwrap_or(false) {
            cmd.arg("--root").arg(root.unwrap_or("/"));
        }
        cmd.stdout(Stdio::piped()).stderr(Stdio::null()).kill_on_drop(true);
        let Ok(out) = tokio::time::timeout(Duration::from_secs(8), cmd.output()).await else { return HashMap::new() };
        let Ok(out) = out else { return HashMap::new() };
        let mut map = HashMap::new();
        for l in String::from_utf8_lossy(&out.stdout).lines() {
            let Ok(v) = serde_json::from_str::<Value>(l) else { continue };
            let (Some(msg), Some(us)) = (v["MESSAGE"].as_str(), v["__REALTIME_TIMESTAMP"].as_str().and_then(|s| s.parse::<i64>().ok())) else { continue };
            // "(user) CMD (command)"
            let Some(rest) = msg.strip_prefix('(') else { continue };
            let Some((user, after)) = rest.split_once(") CMD (") else { continue };
            let command = after.strip_suffix(')').unwrap_or(after).to_string();
            let e = map.entry((user.to_string(), command)).or_insert(0);
            *e = (*e).max(us / 1000);
        }
        map
    }

    fn read(p: &Path) -> Option<String> {
        std::fs::read_to_string(p).ok()
    }

    pub async fn cron_items(root: Option<&str>) -> (Vec<Value>, Vec<String>) {
        let mut entries: Vec<CronEntry> = Vec::new();
        let mut notes = Vec::new();
        if let Some(t) = read(&host_path(root, "/etc/crontab")) {
            entries.extend(parse_crontab(&t, "/etc/crontab", true, "root"));
        }
        if let Ok(rd) = std::fs::read_dir(host_path(root, "/etc/cron.d")) {
            let mut files: Vec<_> = rd.flatten().filter(|e| e.path().is_file()).collect();
            files.sort_by_key(|e| e.file_name());
            for e in files {
                let name = e.file_name().to_string_lossy().to_string();
                if name.starts_with('.') || name.ends_with(".dpkg-old") || name.ends_with(".dpkg-dist") || name.ends_with('~') || name == "placeholder" {
                    continue;
                }
                if let Some(t) = read(&e.path()) {
                    entries.extend(parse_crontab(&t, &format!("/etc/cron.d/{name}"), true, "root"));
                }
            }
        }
        // User crontabs: Debian keeps them in /var/spool/cron/crontabs, Red Hat and macOS elsewhere. Readable only by root.
        let mut unreadable = false;
        for dir in ["/var/spool/cron/crontabs", "/var/spool/cron", "/usr/lib/cron/tabs"] {
            let p = host_path(root, dir);
            match std::fs::read_dir(&p) {
                Ok(rd) => {
                    for e in rd.flatten() {
                        if !e.path().is_file() {
                            continue;
                        }
                        let user = e.file_name().to_string_lossy().to_string();
                        match read(&e.path()) {
                            Some(t) => entries.extend(parse_crontab(&t, &format!("{dir}/{user}"), false, &user)),
                            None => unreadable = true,
                        }
                    }
                }
                Err(e) if e.kind() == std::io::ErrorKind::PermissionDenied => unreadable = true,
                Err(_) => {}
            }
        }
        if unreadable {
            notes.push("User crontabs (crontab -e) are only readable by root. This agent runs unprivileged, so only system cron files and timers are shown.".to_string());
        }
        // Scripts in /etc/cron.{hourly,daily,weekly,monthly}.
        let mut scripts: Vec<(String, String, String)> = Vec::new();
        for period in ["hourly", "daily", "weekly", "monthly"] {
            let dir = format!("/etc/cron.{period}");
            if let Ok(rd) = std::fs::read_dir(host_path(root, &dir)) {
                let mut names: Vec<String> = rd.flatten().filter(|e| e.path().is_file()).map(|e| e.file_name().to_string_lossy().to_string()).filter(|n| !n.starts_with('.') && n != "0anacron" && n != "placeholder").collect();
                names.sort();
                for n in names {
                    scripts.push((period.to_string(), format!("{dir}/{n}"), n));
                }
            }
        }
        let offset = local_offset_secs().await;
        let now_local = now_secs() + offset;
        let last = last_runs(root).await;
        let mut items = Vec::new();
        for e in entries {
            // The run-parts lines of /etc/crontab only trigger the cron.* directories listed below.
            if e.command.contains("run-parts") && e.command.contains("/etc/cron.") {
                continue;
            }
            let Some(x) = CronExpr::parse(&e.schedule) else { continue };
            let next = x.next_runs(now_local, 5).into_iter().map(|t| (t - offset) * 1000).collect::<Vec<_>>();
            let last_run = last.get(&(e.user.clone(), e.command.clone())).copied();
            let short: String = e.command.split_whitespace().next().map(|c| c.rsplit('/').next().unwrap_or(c).to_string()).unwrap_or_default();
            items.push(json!({
                "id": format!("cron:{}:{}", e.source, e.line),
                "kind": "cron", "name": short, "command": e.command, "user": e.user,
                "schedule": e.schedule, "summary": x.describe(),
                "source": format!("{}:{}", e.source, e.line),
                "enabled": true, "state": if x.reboot { "at boot" } else { "scheduled" },
                "next_run": next.first(), "next_runs": next, "last_run": last_run,
            }));
        }
        for (period, path, name) in scripts {
            items.push(json!({
                "id": format!("cron:{path}"), "kind": "cron", "name": name, "command": path, "user": "root",
                "schedule": format!("@{period}"), "summary": format!("Run by cron.{period} (run-parts / anacron)"),
                "source": path, "enabled": true, "state": "scheduled", "next_run": null, "next_runs": [], "last_run": null,
            }));
        }
        (items, notes)
    }

    fn now_secs() -> i64 {
        std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH).map(|d| d.as_secs() as i64).unwrap_or(0)
    }
}

#[cfg(unix)]
pub use unix_impl::cron_unit;

pub async fn list(services: &Services, host_root: Option<&str>) -> Result<Value> {
    #[cfg(unix)]
    let (mut items, mut notes, cron_service) = {
        let (i, n) = unix_impl::cron_items(host_root).await;
        (i, n, Some(unix_impl::cron_unit(host_root)))
    };
    #[cfg(not(unix))]
    let (mut items, mut notes, cron_service): (Vec<Value>, Vec<String>, Option<&str>) = {
        let _ = host_root;
        (vec![], vec![], None)
    };
    match services.timers().await {
        Ok(mut t) => items.append(&mut t),
        Err(e) => {
            if items.is_empty() {
                return Err(e);
            }
            notes.push(format!("Timers / tasks unavailable: {e}"));
        }
    }
    Ok(json!({ "platform": std::env::consts::OS, "items": items, "notes": notes, "cron_unit": cron_service }))
}

#[cfg(test)]
mod tests {
    use super::*;

    fn runs(spec: &str, after: i64, n: usize) -> Vec<i64> {
        CronExpr::parse(spec).unwrap().next_runs(after, n)
    }

    #[test]
    fn parses_and_rejects() {
        assert!(CronExpr::parse("*/5 * * * *").is_some());
        assert!(CronExpr::parse("0 3 * * mon-fri").is_some());
        assert!(CronExpr::parse("@daily").is_some());
        assert!(CronExpr::parse("61 * * * *").is_none());
        assert!(CronExpr::parse("* * * *").is_none());
        assert!(CronExpr::parse("*/0 * * * *").is_none());
    }

    #[test]
    fn next_runs_are_correct() {
        // 2024-01-01 00:00:00 (a Monday) as a naive epoch.
        let t0 = 1_704_067_200;
        assert_eq!(runs("*/15 * * * *", t0, 3), vec![t0 + 900, t0 + 1800, t0 + 2700]);
        assert_eq!(runs("30 2 * * *", t0, 2), vec![t0 + 9000, t0 + 86_400 + 9000]);
        // Weekly on Sunday 04:00: first Sunday after Mon 2024-01-01 is 2024-01-07.
        assert_eq!(runs("0 4 * * 0", t0, 1), vec![t0 + 6 * 86_400 + 4 * 3600]);
        // 1st of the month at 00:00 -> 2024-02-01.
        assert_eq!(runs("0 0 1 * *", t0, 1), vec![t0 + 31 * 86_400]);
        // day-of-month OR day-of-week when both are restricted: Friday 5 Jan beats the 15th.
        assert_eq!(runs("0 0 15 * 5", t0, 1), vec![t0 + 4 * 86_400]);
        assert!(runs("@reboot", t0, 3).is_empty());
    }

    #[test]
    fn leap_day_is_found() {
        let t0 = 1_704_067_200; // 2024-01-01
        let r = runs("0 0 29 2 *", t0, 1);
        assert_eq!(r, vec![t0 + (31 + 28) * 86_400]); // 2024-02-29
    }

    #[test]
    fn descriptions() {
        let d = |s: &str| CronExpr::parse(s).unwrap().describe();
        assert_eq!(d("* * * * *"), "Every minute");
        assert_eq!(d("*/10 * * * *"), "Every 10 minutes");
        assert_eq!(d("0 * * * *"), "Every hour");
        assert_eq!(d("30 2 * * *"), "Every day at 02:30");
        assert_eq!(d("0 4 * * 1"), "Every Monday at 04:00");
        assert_eq!(d("15 6 1 * *"), "Day 1 of every month at 06:15");
        assert_eq!(d("@reboot"), "At system start");
    }

    #[test]
    fn crontab_parsing() {
        let sys = "# comment\nSHELL=/bin/sh\nPATH=/usr/bin\n17 *\t* * *\troot    cd / && run-parts --report /etc/cron.hourly\n*/5 * * * * www-data /usr/bin/php /var/www/cron.php >/dev/null 2>&1\n@daily backup /usr/local/bin/backup.sh\nbad line here\n";
        let e = parse_crontab(sys, "/etc/crontab", true, "root");
        assert_eq!(e.len(), 3);
        assert_eq!(e[1].user, "www-data");
        assert_eq!(e[1].command, "/usr/bin/php /var/www/cron.php >/dev/null 2>&1");
        assert_eq!(e[1].line, 5);
        assert_eq!(e[2].schedule, "@daily");
        assert_eq!(e[2].user, "backup");
        let usr = "MAILTO=me@example.com\n0 1 * * * /home/a/job.sh --flag\n";
        let e = parse_crontab(usr, "/var/spool/cron/crontabs/alice", false, "alice");
        assert_eq!(e.len(), 1);
        assert_eq!(e[0].user, "alice");
        assert_eq!(e[0].command, "/home/a/job.sh --flag");
    }
}
