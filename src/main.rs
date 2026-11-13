//! External inner equijoin of two TSV relations (signed integer key and value).
use std::env;
use std::fs::{self, File, OpenOptions};
use std::io::{self, BufRead, BufReader, BufWriter, Read, Write};
use std::path::{Path, PathBuf};

type Row = (i64, i64);
const FANOUT: usize = 16;
const MAX_DEPTH: usize = 3;
const BUFFER: usize = 8192;

fn invalid(msg: &str) -> io::Error {
    io::Error::new(io::ErrorKind::InvalidInput, msg)
}

struct Scratch(PathBuf);
impl Scratch {
    fn new(parent: &Path) -> io::Result<Self> {
        for n in 0..1000 {
            let path = parent.join(format!(".spillway-{}-{n}", std::process::id()));
            match fs::create_dir(&path) {
                Ok(()) => return Ok(Self(path)),
                Err(e) if e.kind() == io::ErrorKind::AlreadyExists => continue,
                Err(e) => return Err(e),
            }
        }
        Err(invalid("could not allocate a fresh scratch directory"))
    }
}
impl Drop for Scratch {
    fn drop(&mut self) {
        let _ = fs::remove_dir_all(&self.0);
    }
}

fn pack(w: &mut impl Write, row: Row) -> io::Result<()> {
    w.write_all(&row.0.to_le_bytes())?;
    w.write_all(&row.1.to_le_bytes())
}

fn next_row(r: &mut impl BufRead) -> io::Result<Option<Row>> {
    if r.fill_buf()?.is_empty() {
        return Ok(None);
    }
    let mut buf = [0; 16];
    r.read_exact(&mut buf)?;
    Ok(Some((
        i64::from_le_bytes(buf[..8].try_into().unwrap()),
        i64::from_le_bytes(buf[8..].try_into().unwrap()),
    )))
}

fn ingest(input: &Path, dest: &Path) -> io::Result<u64> {
    let mut r = BufReader::new(File::open(input)?);
    let mut w = BufWriter::new(File::create(dest)?);
    let mut count = 0;
    let mut line = String::new();
    loop {
        line.clear();
        if (&mut r).take(129).read_line(&mut line)? == 0 {
            break;
        }
        let fields = line.trim_end_matches(['\r', '\n']);
        let parsed = fields
            .split_once('\t')
            .and_then(|(a, b)| Some((a.parse().ok()?, b.parse().ok()?)));
        if line.len() > 128 || parsed.is_none() {
            return Err(invalid(&format!(
                "{} line {}: expected two i64 TSV fields",
                input.display(),
                count + 1
            )));
        }
        pack(&mut w, parsed.unwrap())?;
        count += 1;
    }
    w.flush()?;
    Ok(count)
}

// SplitMix64 finalizer; vary the key mix by recursion level.
fn partition(key: i64, depth: usize) -> usize {
    let mut x = (key as u64).wrapping_add((depth as u64 + 1).wrapping_mul(0x9e3779b97f4a7c15));
    x = (x ^ (x >> 30)).wrapping_mul(0xbf58476d1ce4e5b9);
    x = (x ^ (x >> 27)).wrapping_mul(0x94d049bb133111eb);
    ((x ^ (x >> 31)) as usize) % FANOUT
}

struct Part {
    path: PathBuf,
    rows: u64,
}
fn split(part: &Part, dir: &Path, depth: usize, side: &str) -> io::Result<Vec<Part>> {
    let mut parts = Vec::new();
    let mut writers = Vec::new();
    for i in 0..FANOUT {
        let path = dir.join(format!("{side}-{i}"));
        writers.push(BufWriter::with_capacity(BUFFER, File::create(&path)?));
        parts.push(Part { path, rows: 0 });
    }
    let mut input = BufReader::new(File::open(&part.path)?);
    while let Some(row) = next_row(&mut input)? {
        let index = partition(row.0, depth);
        pack(&mut writers[index], row)?;
        parts[index].rows += 1;
    }
    for mut writer in writers {
        writer.flush()?;
    }
    Ok(parts)
}

#[derive(Default)]
struct Stats {
    output: u64,
    split_passes: usize,
    fallback_pairs: usize,
    peak_rows: usize,
    spill_bytes: u64,
    probe_rows: u64,
}
struct Engine<W: Write> {
    out: W,
    limit: usize,
    stats: Stats,
}

impl<W: Write> Engine<W> {
    fn blocks(&mut self, build: &Part, probe: &Part, swapped: bool) -> io::Result<()> {
        let mut reader = BufReader::new(File::open(&build.path)?);
        let mut block = Vec::with_capacity(self.limit.min(build.rows as usize));
        loop {
            block.clear();
            while block.len() < self.limit {
                match next_row(&mut reader)? {
                    Some(row) => block.push(row),
                    None => break,
                }
            }
            if block.is_empty() {
                break;
            }
            self.stats.peak_rows = self.stats.peak_rows.max(block.len());
            block.sort_unstable();
            let mut input = BufReader::new(File::open(&probe.path)?);
            while let Some((key, v)) = next_row(&mut input)? {
                self.stats.probe_rows += 1;
                let begin = block.partition_point(|r| r.0 < key);
                for &(_, u) in block[begin..].iter().take_while(|r| r.0 == key) {
                    let (left, right) = if swapped { (v, u) } else { (u, v) };
                    writeln!(self.out, "{key}\t{left}\t{right}")?;
                    self.stats.output += 1;
                }
            }
        }
        Ok(())
    }
    fn join(
        &mut self,
        left: &Part,
        right: &Part,
        dir: &Path,
        depth: usize,
        stalled: bool,
    ) -> io::Result<()> {
        let swapped = left.rows > right.rows;
        let (small, large) = if swapped {
            (right, left)
        } else {
            (left, right)
        };
        if small.rows == 0 {
            return Ok(());
        }
        if small.rows <= self.limit as u64 || depth >= MAX_DEPTH || stalled {
            if small.rows > self.limit as u64 {
                self.stats.fallback_pairs += 1;
            }
            return self.blocks(small, large, swapped);
        }
        let children = Scratch::new(dir)?;
        let l = split(left, &children.0, depth, "l")?;
        let r = split(right, &children.0, depth, "r")?;
        self.stats.split_passes += 1;
        self.stats.spill_bytes += (left.rows + right.rows) * 16;
        for (a, b) in l.iter().zip(&r) {
            self.join(
                a,
                b,
                &children.0,
                depth + 1,
                a.rows.min(b.rows) == small.rows,
            )?;
            fs::remove_file(&a.path)?;
            fs::remove_file(&b.path)?;
        }
        Ok(())
    }
}

fn run(left: &Path, right: &Path, output: &Path, limit: usize) -> io::Result<()> {
    if limit == 0 {
        return Err(invalid("build-row budget must be positive"));
    }
    let parent = output
        .parent()
        .filter(|p| !p.as_os_str().is_empty())
        .unwrap_or(Path::new("."));
    if output.try_exists()? {
        return Err(invalid("destination exists"));
    }
    let dir = Scratch::new(parent)?;
    let load = |input, name| -> io::Result<Part> {
        let path = dir.0.join(name);
        Ok(Part {
            rows: ingest(input, &path)?,
            path,
        })
    };
    let l = load(left, "left")?;
    let r = load(right, "right")?;
    let temp = dir.0.join("result");
    let file = OpenOptions::new()
        .create_new(true)
        .write(true)
        .open(&temp)?;
    let mut engine = Engine {
        out: BufWriter::new(file),
        limit,
        stats: Stats::default(),
    };
    engine.join(&l, &r, &dir.0, 0, false)?;
    engine.out.flush()?;
    // Publish only the fully written inode. Concurrent creators cannot overwrite each other.
    fs::hard_link(temp, output)?;
    let s = engine.stats;
    println!("{{\"left_rows\":{},\"right_rows\":{},\"output_rows\":{},\"peak_build_rows\":{},\"partition_passes\":{},\"fallback_pairs\":{},\"partition_bytes_written\":{},\"probe_rows_scanned\":{}}}", l.rows,r.rows,s.output,s.peak_rows,s.split_passes,s.fallback_pairs,s.spill_bytes,s.probe_rows);
    Ok(())
}
fn main() {
    let args: Vec<_> = env::args_os().skip(1).collect();
    let res = match args.as_slice() {
        [l, r, o, n] => n
            .to_str()
            .and_then(|v| v.parse().ok())
            .ok_or_else(|| invalid("invalid budget"))
            .and_then(|budget| run(Path::new(l), Path::new(r), Path::new(o), budget)),
        _ => Err(invalid(
            "usage: spillway-join LEFT.tsv RIGHT.tsv NEW_OUTPUT.tsv BUILD_ROWS",
        )),
    };
    if let Err(e) = res {
        eprintln!("spillway-join: {e}");
        std::process::exit(1);
    }
}
