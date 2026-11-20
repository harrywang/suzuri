//! Records raw frames from a running Tinymist preview server.
//!
//! Used to capture test fixtures for the headless renderer:
//!
//! ```sh
//! tinymist preview /tmp/doc.typ --no-open \
//!   --data-plane-host 127.0.0.1:23625 --control-plane-host 127.0.0.1:23626
//! cargo run -p typst_live_preview --example record_frames -- \
//!   --out target/preview-frames --count 2 --edit /tmp/doc.typ
//! ```
//!
//! The first frame is the full `new,` document sent on connect. With `--edit`,
//! the file is touched after that frame so a second, incremental `diff-v1,`
//! frame is recorded too.

use std::{
    fs,
    path::{Path, PathBuf},
    sync::{Arc, atomic::AtomicBool, mpsc},
    time::{Duration, Instant},
};

use anyhow::{Context as _, Result, bail};
use typst_live_preview::{
    connection::{self, PreviewConnectionConfig, PreviewEvent},
    protocol::{PreviewDataFrame, split_preview_data_frame},
};

struct Args {
    url: String,
    origin: Option<String>,
    out: PathBuf,
    count: usize,
    edit: Option<PathBuf>,
    timeout: Duration,
}

fn main() -> Result<()> {
    let args = parse_args()?;
    fs::create_dir_all(&args.out).with_context(|| format!("creating {:?}", args.out))?;

    let config = match &args.origin {
        Some(origin) => PreviewConnectionConfig {
            url: args.url.clone(),
            origin: origin.clone(),
        },
        None => PreviewConnectionConfig::loopback(connection::DEFAULT_DATA_PLANE_PORT)
            .with_url(args.url.clone()),
    };

    let (tx, rx) = mpsc::channel();
    let (_outbound_tx, outbound_rx) = tokio::sync::mpsc::unbounded_channel();
    std::thread::spawn(move || {
        let stop = Arc::new(AtomicBool::new(false));
        if let Err(error) = connection::run(config, tx, outbound_rx, stop) {
            eprintln!("connection stopped: {error}");
        }
    });

    let deadline = Instant::now() + args.timeout;
    let mut recorded = 0;
    let mut edited = false;

    while recorded < args.count {
        let remaining = deadline.saturating_duration_since(Instant::now());
        if remaining.is_zero() {
            bail!(
                "timed out after {:?} with {recorded}/{} frame(s) recorded",
                args.timeout,
                args.count
            );
        }

        match rx.recv_timeout(remaining) {
            Ok(PreviewEvent::Connected) => eprintln!("connected to {}", args.url),
            Ok(PreviewEvent::ConnectFailed(error)) => eprintln!("connection failed: {error}"),
            Ok(PreviewEvent::Text(text)) => eprintln!("text frame ({} bytes): {text}", text.len()),
            Ok(PreviewEvent::Binary(bytes)) => {
                let kind = match split_preview_data_frame(&bytes) {
                    Some(PreviewDataFrame::DiffV1(_)) => "diff-v1",
                    Some(PreviewDataFrame::FullCurrent(_)) => "new",
                    None => "unknown",
                };
                let path = args.out.join(format!("frame-{recorded:03}.bin"));
                fs::write(&path, &bytes).with_context(|| format!("writing {path:?}"))?;
                println!(
                    "frame {recorded}: {kind}, {} bytes -> {}",
                    bytes.len(),
                    path.display()
                );
                recorded += 1;

                if !edited && let Some(file) = &args.edit {
                    append_edit(file)?;
                    edited = true;
                }
            }
            Err(mpsc::RecvTimeoutError::Timeout) => continue,
            Err(mpsc::RecvTimeoutError::Disconnected) => bail!("connection task stopped early"),
        }
    }

    Ok(())
}

fn append_edit(path: &Path) -> Result<()> {
    let mut source = fs::read_to_string(path).with_context(|| format!("reading {path:?}"))?;
    source.push_str("\n#text[Recorded edit.]\n");
    fs::write(path, source).with_context(|| format!("writing {path:?}"))?;
    eprintln!("appended an edit to {}", path.display());
    Ok(())
}

fn parse_args() -> Result<Args> {
    let mut args = std::env::args().skip(1);
    let mut url = format!("ws://127.0.0.1:{}", connection::DEFAULT_DATA_PLANE_PORT);
    let mut origin = None;
    let mut out = PathBuf::from("target/preview-frames");
    let mut count = 1;
    let mut edit = None;
    let mut timeout = Duration::from_secs(30);

    while let Some(argument) = args.next() {
        match argument.as_str() {
            "--url" => url = next_value(&mut args, "--url")?,
            "--origin" => origin = Some(next_value(&mut args, "--origin")?),
            "--out" => out = PathBuf::from(next_value(&mut args, "--out")?),
            "--edit" => edit = Some(PathBuf::from(next_value(&mut args, "--edit")?)),
            "--count" => count = next_value(&mut args, "--count")?.parse()?,
            "--timeout" => {
                timeout = Duration::from_secs(next_value(&mut args, "--timeout")?.parse()?)
            }
            "--help" | "-h" => {
                println!(
                    "record_frames [--url URL] [--origin ORIGIN] [--out DIR] [--count N] \
                     [--edit FILE] [--timeout SECONDS]"
                );
                std::process::exit(0);
            }
            other => bail!("unexpected argument {other:?} (try --help)"),
        }
    }

    anyhow::ensure!(count > 0, "--count must be at least 1");

    Ok(Args {
        url,
        origin,
        out,
        count,
        edit,
        timeout,
    })
}

fn next_value(args: &mut impl Iterator<Item = String>, flag: &str) -> Result<String> {
    args.next().with_context(|| format!("{flag} needs a value"))
}
