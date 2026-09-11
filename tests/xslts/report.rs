//! Result bookkeeping and reporting: per test-set, per feature family, per execution profile,
//! not-run reasons, and the W3C results XML (`http://www.w3.org/2012/11/xslt30-test-results`).
//!
//! The headline number is the basic-profile, non-regex pass rate (design sketch §2.6): the two
//! regex families are 27% of the applicable cases and exercise `regexml`, not XSLT.

use std::collections::BTreeMap;
use std::io::Write;
use std::path::Path;

#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub enum Status {
    Pass,
    Fail,
    WrongError,
    HarnessError,
    /// Applicable, but a dependency is not satisfied.
    NotRun,
    /// Not an XSLT 2.0 test (spec dependency).
    NotApplicable,
    /// On the exclusion list.
    Excluded,
}

impl Status {
    pub fn w3c_name(self) -> &'static str {
        match self {
            Status::Pass => "pass",
            Status::Fail | Status::HarnessError => "fail",
            Status::WrongError => "wrongError",
            Status::NotRun | Status::NotApplicable | Status::Excluded => "notRun",
        }
    }
}

#[derive(Debug, Clone)]
pub struct CaseRecord {
    pub set: String,
    pub name: String,
    pub family: String,
    pub profile: &'static str,
    pub status: Status,
    pub message: Option<String>,
    pub micros: u128,
}

#[derive(Debug, Default, Clone)]
pub struct Counts {
    pub total: usize,
    pub applicable: usize,
    pub runnable: usize,
    pub pass: usize,
    pub fail: usize,
    pub wrong_error: usize,
    pub harness_error: usize,
    pub not_run: usize,
    pub excluded: usize,
}

impl Counts {
    fn add(&mut self, s: Status) {
        self.total += 1;
        if s != Status::NotApplicable {
            self.applicable += 1;
        }
        match s {
            Status::Pass => {
                self.runnable += 1;
                self.pass += 1
            }
            Status::Fail => {
                self.runnable += 1;
                self.fail += 1
            }
            Status::WrongError => {
                self.runnable += 1;
                self.wrong_error += 1
            }
            Status::HarnessError => {
                self.runnable += 1;
                self.harness_error += 1
            }
            Status::NotRun => self.not_run += 1,
            Status::Excluded => self.excluded += 1,
            Status::NotApplicable => {}
        }
    }
    pub fn rate(&self) -> f64 {
        if self.runnable == 0 {
            0.0
        } else {
            100.0 * self.pass as f64 / self.runnable as f64
        }
    }
}

#[derive(Default)]
pub struct Report {
    pub records: Vec<CaseRecord>,
    pub by_set: BTreeMap<String, Counts>,
    pub by_family: BTreeMap<String, Counts>,
    pub by_profile: BTreeMap<&'static str, Counts>,
    pub not_run_reasons: BTreeMap<String, usize>,
    pub totals: Counts,
    pub engine: String,
    pub engine_version: String,
    pub suite_version: String,
    pub schema_aware: bool,
    pub seconds: f64,
}

/// Feature family from the catalog path `tests/<family>/<set>/…`; `misc` is split into the two
/// regex sets and the rest.
pub fn family_of(rel_file: &str, set_name: &str) -> String {
    let family = rel_file.split('/').nth(1).unwrap_or("?");
    if family == "misc" {
        if set_name.contains("regex") || set_name.starts_with("unicode") {
            "misc/regex".to_string()
        } else {
            "misc/other".to_string()
        }
    } else {
        family.to_string()
    }
}

impl Report {
    pub fn record(&mut self, r: CaseRecord) {
        self.totals.add(r.status);
        self.by_set.entry(r.set.clone()).or_default().add(r.status);
        self.by_family.entry(r.family.clone()).or_default().add(r.status);
        if r.status != Status::NotApplicable {
            self.by_profile.entry(r.profile).or_default().add(r.status);
        }
        if r.status == Status::NotRun {
            *self.not_run_reasons.entry(r.message.clone().unwrap_or_default()).or_default() += 1;
        }
        self.records.push(r);
    }

    /// Basic-profile, non-regex pass rate (the headline number).
    pub fn headline(&self) -> (usize, usize) {
        let mut pass = 0;
        let mut runnable = 0;
        for r in &self.records {
            if r.profile == "basic" && r.family != "misc/regex" {
                match r.status {
                    Status::Pass => {
                        pass += 1;
                        runnable += 1
                    }
                    Status::Fail | Status::WrongError | Status::HarnessError => runnable += 1,
                    _ => {}
                }
            }
        }
        (pass, runnable)
    }

    pub fn print_summary(&self) {
        let hdr = format!("{:<34} {:>8} {:>6} {:>6} {:>8} {:>10} {:>7} {:>7}", "", "runnable", "pass", "fail", "wrongErr", "harnessErr", "notRun", "rate");
        println!("\n== Per test-set (applicable cases only) ==");
        println!("{hdr}");
        for (name, c) in &self.by_set {
            if c.applicable > 0 {
                print_row(name, c);
            }
        }
        println!("\n== Per feature family ==");
        println!("{hdr}");
        for (name, c) in &self.by_family {
            if c.applicable > 0 {
                print_row(name, c);
            }
        }
        println!("\n== Per execution profile ==");
        println!("{hdr}");
        for (name, c) in &self.by_profile {
            print_row(name, c);
        }
        if !self.not_run_reasons.is_empty() {
            println!("\n== notRun reasons ==");
            let mut reasons: Vec<_> = self.not_run_reasons.iter().collect();
            reasons.sort_by(|a, b| b.1.cmp(a.1).then(a.0.cmp(b.0)));
            for (reason, n) in reasons {
                println!("  {n:6}  {reason}");
            }
        }
        let t = &self.totals;
        let (hp, hr) = self.headline();
        println!("\n== Totals ({} {}, suite {}, {}) ==", self.engine, self.engine_version, self.suite_version, if self.schema_aware { "schema-aware" } else { "basic" });
        println!("  test cases seen:          {}", t.total);
        println!("  XSLT 2.0-applicable:      {}", t.applicable);
        println!("  excluded (documented):    {}", t.excluded);
        println!("  not run (dependencies):   {}", t.not_run);
        println!("  runnable:                 {}", t.runnable);
        println!("  pass / fail / wrongError / harnessError: {} / {} / {} / {}", t.pass, t.fail, t.wrong_error, t.harness_error);
        println!("  pass rate (runnable):     {:.2}%", t.rate());
        println!(
            "  headline (basic, non-regex): {} / {} = {:.2}%",
            hp,
            hr,
            if hr == 0 { 0.0 } else { 100.0 * hp as f64 / hr as f64 }
        );
        println!("  elapsed:                  {:.1}s", self.seconds);
    }

    /// Write results in the suite's own report format so `report/*.xsl` can render them.
    pub fn write_w3c_xml(&self, path: &Path) -> std::io::Result<()> {
        let mut f = std::fs::File::create(path)?;
        writeln!(f, r#"<?xml version="1.0" encoding="UTF-8"?>"#)?;
        writeln!(f, r#"<test-suite-result xmlns="http://www.w3.org/2012/11/xslt30-test-results">"#)?;
        writeln!(f, r#"   <implementation name="{}" version="{}" anonymous-result-column="false">"#, esc(&self.engine), esc(&self.engine_version))?;
        writeln!(f, r#"      <organization name="xsd-schema / xslt2" anonymous="false"/>"#)?;
        writeln!(f, r#"      <submitter name="xslts harness"/>"#)?;
        writeln!(f, r#"      <configuration lang="XT20" schema-aware="{}"/>"#, self.schema_aware)?;
        writeln!(f, r#"   </implementation>"#)?;
        writeln!(f, r#"   <test-run dateRun="{}" testsuiteVersion="{}"/>"#, today(), esc(&self.suite_version))?;
        let mut current: Option<&str> = None;
        for r in &self.records {
            if current != Some(r.set.as_str()) {
                if current.is_some() {
                    writeln!(f, "   </test-set>")?;
                }
                writeln!(f, r#"   <test-set name="{}">"#, esc(&r.set))?;
                current = Some(r.set.as_str());
            }
            let comment = match r.status {
                Status::NotRun => Some(format!("dependency not satisfied: {}", r.message.clone().unwrap_or_default())),
                Status::NotApplicable => Some(format!("spec dependency: {}", r.message.clone().unwrap_or_default())),
                Status::Excluded => Some(format!("excluded: {}", r.message.clone().unwrap_or_default())),
                Status::HarnessError => Some(format!("harness: {}", r.message.clone().unwrap_or_default())),
                Status::Fail | Status::WrongError => r.message.clone(),
                Status::Pass => None,
            };
            match comment {
                Some(c) => writeln!(f, r#"      <test-case name="{}" result="{}" comment="{}"/>"#, esc(&r.name), r.status.w3c_name(), esc(&c))?,
                None => writeln!(f, r#"      <test-case name="{}" result="{}"/>"#, esc(&r.name), r.status.w3c_name())?,
            }
        }
        if current.is_some() {
            writeln!(f, "   </test-set>")?;
        }
        writeln!(f, "</test-suite-result>")?;
        Ok(())
    }
}

fn print_row(name: &str, c: &Counts) {
    println!(
        "{:<34} {:>8} {:>6} {:>6} {:>8} {:>10} {:>7} {:>6.1}%",
        clip(name, 34),
        c.runnable,
        c.pass,
        c.fail,
        c.wrong_error,
        c.harness_error,
        c.not_run,
        c.rate()
    );
}

fn clip(s: &str, n: usize) -> String {
    if s.chars().count() <= n {
        s.to_string()
    } else {
        s.chars().take(n - 1).chain(std::iter::once('…')).collect()
    }
}

fn esc(s: &str) -> String {
    let mut out = String::with_capacity(s.len());
    for ch in s.chars() {
        match ch {
            '&' => out.push_str("&amp;"),
            '<' => out.push_str("&lt;"),
            '>' => out.push_str("&gt;"),
            '"' => out.push_str("&quot;"),
            '\n' | '\r' | '\t' => out.push(' '),
            c if (c as u32) < 0x20 => {}
            c => out.push(c),
        }
    }
    out
}

/// Today's date (UTC) as YYYY-MM-DD without a date-time dependency.
fn today() -> String {
    let secs = std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH).map(|d| d.as_secs()).unwrap_or(0);
    let days = (secs / 86_400) as i64;
    // Howard Hinnant's civil_from_days.
    let z = days + 719_468;
    let era = if z >= 0 { z } else { z - 146_096 } / 146_097;
    let doe = z - era * 146_097;
    let yoe = (doe - doe / 1_460 + doe / 36_524 - doe / 146_096) / 365;
    let y = yoe + era * 400;
    let doy = doe - (365 * yoe + yoe / 4 - yoe / 100);
    let mp = (5 * doy + 2) / 153;
    let d = doy - (153 * mp + 2) / 5 + 1;
    let m = if mp < 10 { mp + 3 } else { mp - 9 };
    let y = if m <= 2 { y + 1 } else { y };
    format!("{y:04}-{m:02}-{d:02}")
}
