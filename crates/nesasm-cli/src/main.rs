use nesasm_core::{
    AssembleOptions, AssembleRequest, AssembleResult, ListLevel, ReferenceTopic, SourceEncoding,
};
use std::{
    collections::BTreeMap,
    env, fs,
    path::PathBuf,
    time::{Duration, SystemTime},
};

struct Arguments {
    request: AssembleRequest,
    output: Option<PathBuf>,
    json: bool,
    check: bool,
    watch: bool,
    usage: usize,
}
/// Maximum number of NES_INCLUDE directories, as in the C# version.
const INCLUDE_LIMIT: usize = 10;

fn parse(args: Vec<String>) -> Result<Option<Arguments>, String> {
    let mut options = AssembleOptions::default();
    let mut input = None;
    let mut output = None;
    let mut includes = Vec::new();
    let mut json = false;
    let mut check = false;
    let mut watch = false;
    let mut usage = 0;
    let mut args = args.into_iter();
    while let Some(arg) = args.next() {
        match arg.as_str() {
            // Checked in order, so `--output -?` still names an output file.
            "-?" | "--help" => return Ok(None),
            "-s" => usage = 1,
            "-S" => usage = 2,
            "-m" => options.macro_listing = true,
            "-raw" => options.raw = true,
            "-autozp" => options.auto_zp = true,
            "-srec" => options.srec = true,
            "-wd" => options.warning_disabled = true,
            "-watch" => watch = true,
            "--json" => json = true,
            "--check" => check = true,
            "--output" => output = Some(PathBuf::from(args.next().ok_or("Missing output path")?)),
            "--include" => includes.push(PathBuf::from(args.next().ok_or("Missing include path")?)),
            "-e" => {
                options.encoding = match args
                    .next()
                    .ok_or("Missing encoding")?
                    .to_ascii_uppercase()
                    .as_str()
                {
                    "UTF8" => SourceEncoding::Utf8,
                    "SJIS" => SourceEncoding::Sjis,
                    _ => return Err("Encoding must be UTF8 or SJIS".into()),
                }
            }
            "-l" => {
                let n = args
                    .next()
                    .ok_or("Missing listing level")?
                    .parse::<i64>()
                    .map_err(|_| "Invalid listing level")?;
                options.list_level = ListLevel::clamped(n);
            }
            "-l0" => options.list_level = ListLevel::Off,
            "-l1" => options.list_level = ListLevel::Brief,
            "-l2" => options.list_level = ListLevel::Normal,
            "-l3" => options.list_level = ListLevel::Full,
            _ if arg.starts_with('-') => return Err(format!("Unknown option '{arg}'")),
            _ => {
                if input.is_some() {
                    return Err("Only one input file is accepted".into());
                }
                input = Some(PathBuf::from(arg));
            }
        }
    }
    let mut input = input.ok_or("Need input file")?;
    if !input
        .extension()
        .is_some_and(|s| s.eq_ignore_ascii_case("asm"))
    {
        input = PathBuf::from(format!("{}.asm", input.display()));
    }
    if let Some(paths) = env::var("NES_INCLUDE").ok().filter(|p| !p.is_empty()) {
        let dirs = include_dirs(&paths);
        if dirs.len() > INCLUDE_LIMIT {
            eprintln!(
                "warning: NES_INCLUDE has {} directories; only the first {INCLUDE_LIMIT} are used",
                dirs.len()
            );
        }
        includes.extend(dirs.into_iter().take(INCLUDE_LIMIT));
    }
    if watch && json {
        return Err("--json cannot be combined with -watch".into());
    }
    Ok(Some(Arguments {
        request: AssembleRequest {
            input,
            working_directory: env::current_dir().map_err(|e| e.to_string())?,
            include_paths: includes,
            allowed_root: None,
            options,
            ..Default::default()
        },
        output,
        json,
        check,
        watch,
        usage,
    }))
}
fn run(args: &Arguments) -> AssembleResult {
    let result = if args.check {
        nesasm_core::assemble(&args.request)
    } else {
        let never = std::sync::atomic::AtomicBool::new(false);
        nesasm_core::build(&args.request, args.output.as_deref(), &never).0
    };
    if args.json {
        println!(
            "{}",
            serde_json::to_string(&result).expect("result serialization")
        );
    } else {
        for d in &result.diagnostics {
            eprintln!(
                "{}:{}: {} [{}]: {}",
                d.location.file.display(),
                d.location.line,
                d.severity,
                d.code,
                d.message
            );
        }
        if result.success {
            println!("Assembled {} bytes", result.binary.len());
        }
        print_regions(&result);
        if args.usage > 0 && result.success {
            print_segment_usage(&result, args.usage > 1);
        }
    }
    result
}
/// Region report in the C# format.
fn print_regions(result: &AssembleResult) {
    if result.regions.is_empty() {
        return;
    }
    println!("==================== Region Info ====================");
    for r in result.regions.values() {
        match (r.begin, r.end, r.size) {
            (None, _, _) => println!("Region {:<12}: BEGINREGION not found", r.name),
            (_, None, _) | (_, _, None) => println!("Region {:<12}: ENDREGION not found", r.name),
            (_, _, Some(size)) => println!(
                "Region {:<12}: {size:>8} bytes (0x{:06X} bytes)",
                r.name, size as u32
            ),
        }
    }
    println!("=====================================================");
}
/// Segment usage table in the C# format (`-s`; `-S` adds the section runs).
fn print_segment_usage(result: &AssembleResult, detail: bool) {
    const SECTION_NAMES: [&str; 4] = ["  ZP", " BSS", "CODE", "DATA"];
    println!("segment usage:\n");
    let ram = result.ram;
    if ram.zero_page_end <= 1 {
        println!("      ZP    -");
    } else {
        let stop = ram.zero_page_end - 1;
        println!("      ZP    ${:04X}-${stop:04X}  [{:4}]", 0, stop + 1);
    }
    if ram.bss_end <= 0x201 {
        println!("     BSS    -");
    } else {
        let stop = ram.bss_end - 1;
        println!(
            "     BSS    ${:04X}-${stop:04X}  [{:4}]",
            0x200,
            stop - 0x200 + 1
        );
    }
    if result.banks.len() > 1 {
        println!("\t\t\t\t    USED/FREE");
    }
    let (mut used, mut free) = (0, 0);
    for bank in &result.banks {
        let name = bank.name.as_deref().unwrap_or("");
        if bank.used == 0 {
            println!("BANK{:4}    {name:>20}       0/8192", bank.bank);
            continue;
        }
        println!(
            "BANK{:4}    {name:>20}    {:4}/{:4}",
            bank.bank,
            bank.used,
            bank.capacity - bank.used
        );
        used += bank.used;
        free += bank.capacity - bank.used;
        if detail {
            for segment in &bank.segments {
                println!(
                    "    {}    ${:04X}-${:04X}  [{:4}]",
                    SECTION_NAMES[segment.section as usize],
                    segment.start,
                    segment.start + segment.size - 1,
                    segment.size
                );
            }
        }
    }
    println!("\t\t\t\t    ---- ----");
    println!("\t\t\t\t    {:4}K{:4}K", (used + 1023) >> 10, free >> 10);
}
/// Whether a source file changed after `started`, i.e. while or just after it
/// was assembled, before its state was recorded; such a change would otherwise
/// be missed. Directories are skipped: writing the artifacts updates them.
/// Timestamps in the future are ignored.
fn changed_since(
    files: &BTreeMap<PathBuf, Option<(SystemTime, u64)>>,
    started: SystemTime,
) -> bool {
    let now = SystemTime::now();
    files.iter().any(|(path, state)| {
        path.is_file() && state.is_some_and(|(modified, _)| modified >= started && modified <= now)
    })
}
fn fingerprint(paths: &[PathBuf]) -> BTreeMap<PathBuf, Option<(SystemTime, u64)>> {
    paths
        .iter()
        .map(|p| {
            (
                p.clone(),
                fs::metadata(p)
                    .ok()
                    .and_then(|m| m.modified().ok().map(|t| (t, m.len()))),
            )
        })
        .collect()
}
/// Splits NES_INCLUDE. `;` separates entries on every OS, as in the C# version;
/// on other OSes `:` is accepted too.
fn include_dirs(paths: &str) -> Vec<PathBuf> {
    let separators: &[char] = if cfg!(windows) { &[';'] } else { &[';', ':'] };
    paths
        .split(separators)
        .filter(|p| !p.is_empty())
        .map(PathBuf::from)
        .collect()
}
/// The options reference as plain text for the terminal.
fn help_text() -> String {
    ReferenceTopic::Options
        .text()
        .lines()
        .map(|line| line.trim_start_matches("# ").replace('`', ""))
        .collect::<Vec<_>>()
        .join("\n")
}
fn main() {
    let args = match parse(env::args().skip(1).collect()) {
        Ok(Some(a)) => a,
        Ok(None) => {
            println!("{}", help_text());
            return;
        }
        Err(e) => {
            eprintln!("{e}");
            eprintln!("Run 'nesasm --help' for usage.");
            // Usage errors are distinguished from assembly errors (error count).
            std::process::exit(2);
        }
    };
    let mut started = SystemTime::now();
    let mut result = run(&args);
    if args.watch {
        // Keyboard handling uses a separate thread, so polling works on all three OSes.
        let (send, recv) = std::sync::mpsc::channel();
        std::thread::spawn(move || {
            use std::io::BufRead;
            // End of input or a read error ends the loop; the main loop then quits.
            for line in std::io::stdin().lock().lines() {
                let Ok(line) = line else { break };
                if send.send(line).is_err() {
                    break;
                }
            }
        });
        println!(
            "Watching dependencies. H + Enter: help; R + Enter: rebuild; Q + Enter or end of input: quit."
        );
        let mut paths = result.dependencies.clone();
        paths.push(args.request.working_directory.join(&args.request.input));
        // Watch include directories too, to recover from previously missing dependencies.
        paths.push(args.request.working_directory.clone());
        paths.extend(args.request.include_paths.iter().map(|p| {
            if p.is_absolute() {
                p.clone()
            } else {
                args.request.working_directory.join(p)
            }
        }));
        let mut previous = fingerprint(&paths);
        let mut stale = changed_since(&previous, started);
        loop {
            std::thread::sleep(Duration::from_millis(200));
            let key = match recv.try_recv() {
                Ok(key) => key.to_ascii_uppercase(),
                Err(std::sync::mpsc::TryRecvError::Empty) => String::new(),
                Err(std::sync::mpsc::TryRecvError::Disconnected) => break,
            };
            if key == "Q" {
                break;
            }
            if key == "H" {
                println!("H: help, R: rebuild, Q: quit (press Enter after key)");
            }
            let current = fingerprint(&paths);
            if current != previous || stale || key == "R" {
                // Debounce bursts from editors replacing files.
                std::thread::sleep(Duration::from_millis(100));
                started = SystemTime::now();
                result = run(&args);
                for p in result.dependencies {
                    if !paths.contains(&p) {
                        paths.push(p);
                    }
                }
                previous = fingerprint(&paths);
                stale = changed_since(&previous, started);
            }
        }
    } else if !result.success {
        // As in the C# version the exit code is the error count (kept within 1..=255).
        std::process::exit(result.error_count().clamp(1, 255) as i32);
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::path::Path;
    #[test]
    fn legacy_options() {
        let a = parse(vec![
            "-l".into(),
            "9".into(),
            "-e".into(),
            "SJIS".into(),
            "-raw".into(),
            "demo".into(),
        ])
        .unwrap()
        .unwrap();
        assert_eq!(a.request.options.list_level, ListLevel::Full);
        assert!(a.request.options.raw);
        assert_eq!(a.request.input, Path::new("demo.asm"));
    }
    #[test]
    fn missing_argument() {
        assert!(parse(vec!["-e".into()]).is_err());
    }
    #[test]
    fn help_is_recognized_only_as_an_option() {
        assert!(parse(vec!["-?".into()]).unwrap().is_none());
        assert!(
            parse(vec!["demo".into(), "--help".into()])
                .unwrap()
                .is_none()
        );
        let a = parse(vec!["--output".into(), "-?".into(), "demo".into()])
            .unwrap()
            .unwrap();
        assert_eq!(a.output.as_deref(), Some(Path::new("-?")));
    }
    #[test]
    fn include_directories_accept_semicolons() {
        assert_eq!(
            include_dirs("a;b;;c"),
            ["a", "b", "c"].map(PathBuf::from).to_vec()
        );
        if !cfg!(windows) {
            assert_eq!(include_dirs("a:b").len(), 2);
        }
    }
}
