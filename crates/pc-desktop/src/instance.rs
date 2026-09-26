//! Per-profile instance ownership, acquired before opening any database.
//! The OS lock, not the presence of its file, determines ownership. A
//! replacement waits for the old owner's coordinated shutdown; ordinary
//! launches only ask the owner to show its window and never open a database.
use std::io::{Read, Write};
use std::net::{SocketAddr, TcpListener, TcpStream};
use std::path::Path;
use std::time::{Duration, Instant};

pub enum Claim {
    Owner(Instance),
    Activated,
}

pub struct Instance {
    _lock: pc_core::lock::WriterLock,
    listener: TcpListener,
}

impl Instance {
    pub fn claim(dir: &Path, replacement: bool, timeout: Duration) -> anyhow::Result<Claim> {
        std::fs::create_dir_all(dir)?;
        let key = dir.join("desktop-instance");
        let endpoint = dir.join("desktop-instance.port");
        let deadline = Instant::now() + timeout;
        loop {
            match pc_core::lock::take_writer(&key, "desktop") {
                Ok(guard) => {
                    let listener = TcpListener::bind("127.0.0.1:0")?;
                    listener.set_nonblocking(true)?;
                    std::fs::write(&endpoint, listener.local_addr()?.port().to_string())?;
                    return Ok(Claim::Owner(Self {
                        _lock: guard,
                        listener,
                    }));
                }
                Err(e) if e.is::<pc_core::lock::Busy>() => {
                    if !replacement && activate(&endpoint) {
                        return Ok(Claim::Activated);
                    }
                    if Instant::now() >= deadline {
                        return Err(std::io::Error::new(std::io::ErrorKind::TimedOut,
                            "the existing desktop instance is not responding; no second server was started").into());
                    }
                    std::thread::sleep(Duration::from_millis(50));
                }
                Err(e) => return Err(e),
            }
        }
    }

    /// Called from the shell's listener thread. Only an activation is
    /// accepted: no paths, arguments, scripts or archive operations.
    pub fn activated(&self) -> bool {
        let Ok((mut stream, _)) = self.listener.accept() else {
            return false;
        };
        let _ = stream.set_read_timeout(Some(Duration::from_millis(200)));
        let _ = stream.set_write_timeout(Some(Duration::from_millis(200)));
        let mut request = [0; 4];
        if stream.read_exact(&mut request).is_ok() && &request == b"show" {
            let _ = stream.write_all(b"ok");
            true
        } else {
            false
        }
    }
}

fn activate(endpoint: &Path) -> bool {
    let port = std::fs::read_to_string(endpoint)
        .ok()
        .and_then(|s| s.parse::<u16>().ok());
    let Some(port) = port else { return false };
    let addr = SocketAddr::from(([127, 0, 0, 1], port));
    let Ok(mut stream) = TcpStream::connect_timeout(&addr, Duration::from_millis(200)) else {
        return false;
    };
    let _ = stream.set_read_timeout(Some(Duration::from_millis(500)));
    let _ = stream.set_write_timeout(Some(Duration::from_millis(200)));
    let mut response = [0; 2];
    stream.write_all(b"show").is_ok()
        && stream.read_exact(&mut response).is_ok()
        && &response == b"ok"
}
