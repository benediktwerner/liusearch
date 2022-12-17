use std::{
    fs::File,
    io::{BufRead, BufReader, BufWriter, Read, Write},
};

use clap::{Parser, Subcommand, ValueEnum};
use rustc_hash::FxHashSet;

#[derive(Parser)]
struct Args {
    #[clap(subcommand)]
    command: Command,
    #[clap(short, long, value_parser, default_value_t = 100)]
    progress: u64,
}

#[derive(Subcommand)]
enum Command {
    Download {
        #[clap(value_parser, rename_all = "lower")]
        variant: Variant,
        #[clap(value_parser)]
        year: u32,
        #[clap(value_parser)]
        month: u32,
        #[clap(value_parser, requires = "month_to")]
        year_to: Option<u32>,
        #[clap(value_parser)]
        month_to: Option<u32>,
    },
    Extract {
        #[clap(value_parser)]
        file: String,
    },
}

#[derive(Clone, Copy, PartialEq, Eq, ValueEnum)]
enum Variant {
    All,
    Standard,
    Exotic,
    Antichess,
    Atomic,
    Chess960,
    Horde,
    Koth,
    ThreeCheck,
    Racing,
    Zh,
}

impl Variant {
    const fn names(self) -> &'static [&'static str] {
        match self {
            Variant::All => &[
                "standard",
                "antichess",
                "atomic",
                "chess960",
                "horde",
                "kingOfTheHill",
                "threeCheck",
                "racingKings",
                "crazyhouse",
            ],
            Variant::Standard => &["standard"],
            Variant::Exotic => &[
                "antichess",
                "atomic",
                "chess960",
                "horde",
                "kingOfTheHill",
                "threeCheck",
                "racingKings",
                "crazyhouse",
            ],
            Variant::Antichess => &["antichess"],
            Variant::Atomic => &["atomic"],
            Variant::Chess960 => &["chess960"],
            Variant::Horde => &["horde"],
            Variant::Koth => &["kingOfTheHill"],
            Variant::ThreeCheck => &["threeCheck"],
            Variant::Racing => &["racingKings"],
            Variant::Zh => &["crazyhouse"],
        }
    }
}

fn main() {
    let args = Args::parse();

    match args.command {
        Command::Download {
            variant,
            year,
            month,
            year_to,
            month_to,
        } => {
            for variant in variant.names() {
                if let (Some(year_to), Some(month_to)) = (year_to, month_to) {
                    if year == year_to {
                        for m in month..=month_to {
                            run_download(variant, year, m, args.progress);
                        }
                    } else {
                        for m in month..=12 {
                            run_download(variant, year, m, args.progress);
                        }
                        for y in year + 1..year_to {
                            for m in 1..=12 {
                                run_download(variant, y, m, args.progress);
                            }
                        }
                        for m in 1..=month_to {
                            run_download(variant, year_to, m, args.progress);
                        }
                    }
                } else {
                    run_download(variant, year, month, args.progress);
                }
            }
        }
        Command::Extract { file } => {
            let outfile = file
                .replace(".pgn.bz2", ".txt")
                .replace("lichess_db_", "names-")
                .replace("_rated_", "-");
            let file = File::open(file).unwrap();
            let length = file.metadata().unwrap().len();
            let reader = BufReader::new(file);
            run(reader, length, &outfile, args.progress);
        }
    }
}

fn run_download(variant: &str, year: u32, month: u32, progress_step: u64) {
    println!("{variant} {year} {month}");
    let resp = reqwest::blocking::get(format!(
        "https://database.lichess.org/{variant}/lichess_db_{variant}_rated_{year}-{month:02}.pgn.bz2"
    ))
    .unwrap()
    .error_for_status()
    .unwrap();
    let length = resp.content_length().unwrap();
    let outfile = format!("names-{variant}-{year}-{month:02}.txt");
    run(resp, length, &outfile, progress_step);
}

fn run(reader: impl Read, length: u64, outfile: &str, progress_step: u64) {
    let start = std::time::Instant::now();
    let progress_step = progress_step * 1_000_000;
    let mut names = FxHashSet::default();

    let mut progress = 0;
    let mut nxt_prog = progress_step;
    let progress_reader = ProgressReader::new(reader, |p| {
        progress += p as u64;
        if progress > nxt_prog {
            let elapsed = start.elapsed().as_secs();
            let left = elapsed * (length - progress) / progress;
            println!("{} - {elapsed}s - {left}s", progress * 1000 / length);
            nxt_prog += progress_step;
        }
    });
    let decoder = bzip2::read::MultiBzDecoder::new(progress_reader);
    let reader = BufReader::new(decoder);

    for line in reader.lines() {
        let line = line.unwrap();
        if line.starts_with("[White ") || line.starts_with("[Black ") {
            names.insert(line[8..line.len() - 2].to_string());
        }
    }

    let outfile = File::create(outfile).unwrap();
    let mut writer = BufWriter::new(outfile);
    for n in names {
        writeln!(writer, "{n}").unwrap();
    }
}

struct ProgressReader<R: Read, C: FnMut(usize)> {
    reader: R,
    callback: C,
}

impl<R: Read, C: FnMut(usize)> ProgressReader<R, C> {
    pub const fn new(reader: R, callback: C) -> Self {
        Self { reader, callback }
    }
}

impl<R: Read, C: FnMut(usize)> Read for ProgressReader<R, C> {
    fn read(&mut self, buf: &mut [u8]) -> std::io::Result<usize> {
        let read = self.reader.read(buf)?;
        (self.callback)(read);
        Ok(read)
    }
}
