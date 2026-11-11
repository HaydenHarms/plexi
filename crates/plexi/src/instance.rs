//! One Plexi at a time. The first copy listens on a loopback port and writes the port,
//! with a secret, to `instance` in the data folder. A later launch hands its request
//! (open a file, show the notebook, draw) to that copy and exits, so the hotkeys and the
//! tray icon stay with a single process.

use std::io::{BufRead, BufReader, Write};
use std::net::{Ipv4Addr, SocketAddr, TcpListener, TcpStream};
use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};
use std::time::{Duration, Instant};

const FILE_NAME: &str = "instance";
/// How long a launch waits for a Plexi that's closing (say, restarting for an update)
/// to finish before taking over.
const WAIT_FOR_CLOSING: Duration = Duration::from_secs(15);

/// What a launch asks of the Plexi that's already running.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Request {
    /// Bring up the notebook.
    Show,
    /// Start drawing on the screen.
    Draw,
    /// Open this notebook file.
    Open(PathBuf),
    /// Nothing: it's already running (a second start at login, say).
    Nothing,
}

impl Request {
    pub fn from_start(file: Option<&Path>, overlay: bool, background: bool) -> Self {
        match file {
            Some(f) => Self::Open(std::path::absolute(f).unwrap_or_else(|_| f.to_path_buf())),
            None if overlay => Self::Draw,
            None if background => Self::Nothing,
            None => Self::Show,
        }
    }

    fn encode(&self) -> String {
        match self {
            Self::Show => "show".into(),
            Self::Draw => "draw".into(),
            Self::Nothing => "nothing".into(),
            Self::Open(p) => format!("open\t{}", p.display()),
        }
    }

    fn decode(line: &str) -> Option<Self> {
        match line {
            "show" => Some(Self::Show),
            "draw" => Some(Self::Draw),
            "nothing" => Some(Self::Nothing),
            _ => line.strip_prefix("open\t").filter(|p| !p.is_empty()).map(|p| Self::Open(PathBuf::from(p))),
        }
    }
}

pub enum Claim {
    /// Another Plexi took the request; this one should exit.
    Handed,
    /// This is the only Plexi. `None` if it couldn't listen for later launches.
    Run(Option<Primary>),
}

/// The right to be the one Plexi, before it starts taking requests.
pub struct Primary {
    listener: TcpListener,
    token: String,
    file: PathBuf,
}

/// Taking requests from later launches.
pub struct Listening {
    closing: Arc<AtomicBool>,
    file: PathBuf,
    port: u16,
}

/// Hand `req` to the Plexi already running, or become the one that runs.
pub fn claim(dir: &Path, req: &Request) -> Claim {
    claim_within(dir, req, WAIT_FOR_CLOSING)
}

fn claim_within(dir: &Path, req: &Request, wait: Duration) -> Claim {
    let deadline = Instant::now() + wait;
    loop {
        match hand_over(dir, req) {
            Handover::Done => return Claim::Handed,
            Handover::Closing if Instant::now() < deadline => std::thread::sleep(Duration::from_millis(150)),
            _ => break,
        }
    }
    match become_first(dir) {
        Ok(p) => Claim::Run(Some(p)),
        Err(e) => {
            log::warn!("can't listen for other launches: {e}");
            Claim::Run(None)
        }
    }
}

enum Handover {
    Done,
    Closing,
    NoOne,
}

fn hand_over(dir: &Path, req: &Request) -> Handover {
    let Ok(text) = std::fs::read_to_string(dir.join(FILE_NAME)) else { return Handover::NoOne };
    let mut parts = text.split_whitespace();
    let (Some(port), Some(token)) = (parts.next().and_then(|p| p.parse::<u16>().ok()), parts.next()) else {
        return Handover::NoOne;
    };
    let addr = SocketAddr::from((Ipv4Addr::LOCALHOST, port));
    let Ok(mut s) = TcpStream::connect_timeout(&addr, Duration::from_millis(500)) else { return Handover::NoOne };
    let _ = s.set_read_timeout(Some(Duration::from_secs(3)));
    if write!(s, "{token}\n{}\n", req.encode()).is_err() {
        return Handover::NoOne;
    }
    let mut reply = String::new();
    let _ = BufReader::new(s).read_line(&mut reply);
    match reply.trim() {
        "ok" => Handover::Done,
        "closing" => Handover::Closing,
        _ => Handover::NoOne,
    }
}

fn become_first(dir: &Path) -> std::io::Result<Primary> {
    let listener = TcpListener::bind((Ipv4Addr::LOCALHOST, 0))?;
    let token = secret();
    std::fs::create_dir_all(dir)?;
    let file = dir.join(FILE_NAME);
    let tmp = dir.join(format!("{FILE_NAME}.{}.tmp", std::process::id()));
    std::fs::write(&tmp, format!("{} {token}\n", listener.local_addr()?.port()))?;
    std::fs::rename(&tmp, &file)?;
    Ok(Primary { listener, token, file })
}

/// 128 random bits as hex, so only someone who can read the data folder can send requests.
fn secret() -> String {
    use std::hash::{BuildHasher, Hasher};
    let now = std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH).map(|d| d.as_nanos()).unwrap_or(0);
    (0..2u8)
        .map(|i| {
            let mut h = std::collections::hash_map::RandomState::new().build_hasher();
            h.write_u8(i);
            h.write_u128(now);
            h.write_u32(std::process::id());
            format!("{:016x}", h.finish())
        })
        .collect()
}

impl Primary {
    /// Start taking requests from later launches on a thread of its own; each one is
    /// passed to `deliver`.
    pub fn serve(self, deliver: impl Fn(Request) + Send + 'static) -> Listening {
        let closing = Arc::new(AtomicBool::new(false));
        let port = self.listener.local_addr().map(|a| a.port()).unwrap_or(0);
        let Primary { listener, token, file } = self;
        let flag = closing.clone();
        let spawned = std::thread::Builder::new().name("plexi-instance".into()).spawn(move || {
            for stream in listener.incoming().flatten() {
                answer(stream, &token, &flag, &deliver);
            }
        });
        if let Err(e) = spawned {
            log::warn!("can't listen for other launches: {e}");
        }
        Listening { closing, file, port }
    }
}

fn answer(stream: TcpStream, token: &str, closing: &AtomicBool, deliver: &impl Fn(Request)) {
    let _ = stream.set_read_timeout(Some(Duration::from_secs(2)));
    let Ok(read) = stream.try_clone() else { return };
    let mut lines = BufReader::new(read).lines();
    let (Some(Ok(given)), Some(Ok(line))) = (lines.next(), lines.next()) else { return };
    if given != token {
        return;
    }
    let mut stream = stream;
    if closing.load(Ordering::SeqCst) {
        let _ = stream.write_all(b"closing\n");
        return;
    }
    let Some(req) = Request::decode(&line) else { return };
    deliver(req);
    let _ = stream.write_all(b"ok\n");
}

impl Listening {
    /// Plexi is on its way out: tell later launches to wait and take over instead.
    pub fn stop(&self) {
        self.closing.store(true, Ordering::SeqCst);
    }
}

impl Drop for Listening {
    fn drop(&mut self) {
        self.stop();
        // Leave the file alone if a newer Plexi has already written its own.
        let ours = std::fs::read_to_string(&self.file)
            .is_ok_and(|t| t.split_whitespace().next() == Some(self.port.to_string().as_str()));
        if ours {
            let _ = std::fs::remove_file(&self.file);
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::mpsc::channel;

    fn scratch(name: &str) -> PathBuf {
        let d = std::env::temp_dir().join(format!("plexi-instance-{name}-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&d);
        d
    }

    fn primary(c: Claim) -> Primary {
        match c {
            Claim::Run(Some(p)) => p,
            _ => panic!("expected to be the first"),
        }
    }

    #[test]
    fn requests_survive_the_trip() {
        for r in [Request::Show, Request::Draw, Request::Nothing, Request::Open(PathBuf::from(r"C:\notes\a b.plexi"))] {
            assert_eq!(Request::decode(&r.encode()), Some(r));
        }
        assert_eq!(Request::decode("open\t"), None);
        assert_eq!(Request::decode("format c:"), None);
        assert_eq!(Request::from_start(None, false, false), Request::Show);
        assert_eq!(Request::from_start(None, true, false), Request::Draw);
        assert_eq!(Request::from_start(None, false, true), Request::Nothing);
        assert!(
            matches!(Request::from_start(Some(Path::new("x.plexi")), false, false), Request::Open(p) if p.is_absolute())
        );
    }

    #[test]
    fn a_second_launch_hands_over_to_the_first() {
        let dir = scratch("handover");
        let first = primary(claim(&dir, &Request::Show));
        let (tx, rx) = channel();
        let listening = first.serve(move |r| tx.send(r).unwrap());

        let file = PathBuf::from(r"C:\notes\week 3.plexi");
        assert!(matches!(claim(&dir, &Request::Open(file.clone())), Claim::Handed));
        assert_eq!(rx.recv_timeout(Duration::from_secs(2)).unwrap(), Request::Open(file));
        assert!(matches!(claim(&dir, &Request::Show), Claim::Handed));
        assert_eq!(rx.recv_timeout(Duration::from_secs(2)).unwrap(), Request::Show);

        // Someone without the secret gets nowhere.
        let port: u16 =
            std::fs::read_to_string(dir.join(FILE_NAME)).unwrap().split(' ').next().unwrap().parse().unwrap();
        let mut s = TcpStream::connect((Ipv4Addr::LOCALHOST, port)).unwrap();
        write!(s, "guess\nshow\n").unwrap();
        let mut reply = String::new();
        let _ = BufReader::new(s).read_line(&mut reply);
        assert_eq!(reply, "");
        assert!(rx.recv_timeout(Duration::from_millis(200)).is_err());

        // While it's closing, a launch waits, then takes over once it's gone.
        listening.stop();
        let started = Instant::now();
        let next = primary(claim_within(&dir, &Request::Show, Duration::from_millis(400)));
        assert!(started.elapsed() >= Duration::from_millis(400), "waited for the old one");
        assert!(rx.try_recv().is_err(), "the closing Plexi didn't take it");
        drop(listening); // the old one exits; the new one's file stays
        assert!(dir.join(FILE_NAME).exists());
        let (tx, rx) = channel();
        let _l = next.serve(move |r| tx.send(r).unwrap());
        assert!(matches!(claim(&dir, &Request::Draw), Claim::Handed));
        assert_eq!(rx.recv_timeout(Duration::from_secs(2)).unwrap(), Request::Draw);
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn a_crashed_plexi_doesnt_block_the_next() {
        let dir = scratch("stale");
        // The file points at a port nobody listens on any more.
        let gone = TcpListener::bind((Ipv4Addr::LOCALHOST, 0)).unwrap();
        let port = gone.local_addr().unwrap().port();
        drop(gone);
        std::fs::create_dir_all(&dir).unwrap();
        std::fs::write(dir.join(FILE_NAME), format!("{port} deadbeef\n")).unwrap();
        let started = Instant::now();
        let _p = primary(claim(&dir, &Request::Show));
        assert!(started.elapsed() < Duration::from_secs(2));
        let _ = std::fs::remove_dir_all(&dir);
    }
}
