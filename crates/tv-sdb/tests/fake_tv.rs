//! Drives the client against a scripted in-process sdbd with a tiny
//! negotiated payload, so every transfer is split across many frames.

use std::collections::HashMap;
use std::io::{Read, Write};
use std::net::{TcpListener, TcpStream};
use std::path::Path;
use std::time::Duration;
use tv_sdb::frame::{Frame, CLSE, CNXN, OKAY, OPEN, WRTE};
use tv_sdb::{Device, Options};

const MAX: usize = 1000;

struct Fake {
    s: TcpStream,
    local: u32,
    remote: u32,
    inbox: Vec<u8>,
}

impl Fake {
    fn send(&mut self, f: Frame) {
        self.s.write_all(&f.encode()).unwrap();
    }
    fn write(&mut self, data: &[u8]) {
        for chunk in data.chunks(MAX) {
            self.send(Frame::new(WRTE, self.remote, self.local, chunk));
            assert_eq!(Frame::read(&mut self.s).unwrap().command, OKAY);
        }
    }
    fn take(&mut self, n: usize) -> Vec<u8> {
        while self.inbox.len() < n {
            let f = Frame::read(&mut self.s).unwrap();
            assert_eq!(f.command, WRTE);
            assert!(
                f.data.len() <= MAX,
                "client ignored the negotiated payload size"
            );
            self.inbox.extend_from_slice(&f.data);
            self.send(Frame::new(OKAY, self.remote, self.local, Vec::new()));
        }
        self.inbox.drain(..n).collect()
    }
    fn record(&mut self) -> (Vec<u8>, u32) {
        let h = self.take(8);
        (
            h[..4].to_vec(),
            u32::from_le_bytes(h[4..].try_into().unwrap()),
        )
    }
}

fn rec(id: &[u8], arg: u32, payload: &[u8]) -> Vec<u8> {
    [id, &arg.to_le_bytes(), payload].concat()
}

fn serve(s: TcpStream, files: &mut HashMap<String, Vec<u8>>) {
    let mut fake = Fake {
        s,
        local: 0,
        remote: 0,
        inbox: Vec::new(),
    };
    assert_eq!(Frame::read(&mut fake.s).unwrap().command, CNXN);
    fake.send(Frame::new(
        CNXN,
        0x0200_0000,
        MAX as u32,
        *b"device::FAKE::0\0",
    ));
    while let Ok(open) = Frame::read(&mut fake.s) {
        if open.command != OPEN {
            continue; // the client's CLSE for a finished channel
        }
        fake.local = open.arg0;
        fake.remote += 1;
        let service = String::from_utf8(open.data).unwrap();
        let service = service.trim_end_matches('\0');
        if service == "shell:refuse" {
            fake.send(Frame::new(CLSE, 0, fake.local, Vec::new()));
            continue;
        }
        fake.send(Frame::new(OKAY, fake.remote, fake.local, Vec::new()));
        match service {
            "capability:" => {
                let text = b"cpu_arch:armv7\nsdk_toolpath:/tmp\n\0";
                fake.write(&[&(text.len() as u16).to_le_bytes()[..], text].concat());
            }
            "shell:0 appinstall tpk my_app.tpk" => {
                let staged = files.contains_key("/home/owner/share/tmp/sdk_tools/my_app.tpk");
                fake.write(if staged {
                    b"key[end] val[ok]\r\n"
                } else {
                    b"key[end] val[fail]\r\n"
                });
            }
            s if s.starts_with("shell:") => {
                let out = format!("ran {}\n", &s[6..]).repeat(200);
                fake.write(out.as_bytes());
            }
            "sync:" => loop {
                let (id, len) = fake.record();
                match &id[..] {
                    b"STAT" => {
                        let path = String::from_utf8(fake.take(len as usize)).unwrap();
                        let mode: u32 = if path == "/dir" { 0o040_755 } else { 0 };
                        fake.write(&rec(b"STAT", mode, &[0; 8]));
                    }
                    b"SEND" => {
                        let target = String::from_utf8(fake.take(len as usize)).unwrap();
                        let (path, mode) = target.rsplit_once(',').unwrap();
                        assert_eq!(mode, "33261");
                        let mut data = Vec::new();
                        loop {
                            match fake.record() {
                                (id, n) if id == b"DATA" => data.extend(fake.take(n as usize)),
                                (id, _) if id == b"DONE" => break,
                                other => panic!("unexpected {other:?}"),
                            }
                        }
                        files.insert(path.to_owned(), data);
                        fake.write(&rec(b"OKAY", 0, &[]));
                    }
                    b"RECV" => {
                        let path = String::from_utf8(fake.take(len as usize)).unwrap();
                        match files.get(&path).cloned() {
                            Some(data) => {
                                for chunk in data.chunks(64 * 1024) {
                                    fake.write(&rec(b"DATA", chunk.len() as u32, chunk));
                                }
                                fake.write(&rec(b"DONE", 0, &[]));
                            }
                            None => {
                                let msg = b"You cannot pull files from this path.";
                                fake.write(&rec(b"FAIL", msg.len() as u32, msg));
                                break; // like sdbd, close the channel after FAIL
                            }
                        }
                    }
                    b"QUIT" => break,
                    other => panic!("unexpected record {other:?}"),
                }
            },
            other => panic!("unexpected service {other}"),
        }
        fake.send(Frame::new(CLSE, 0, fake.local, Vec::new()));
    }
}

#[test]
fn end_to_end_against_fake_tv() {
    let listener = TcpListener::bind("127.0.0.1:0").unwrap();
    let target = listener.local_addr().unwrap().to_string();
    std::thread::spawn(move || {
        let mut files = HashMap::new();
        for s in listener.incoming() {
            serve(s.unwrap(), &mut files);
        }
    });
    let options = Options {
        timeout: Duration::from_secs(5),
        verbose: false,
    };
    let mut tv = Device::connect(&target, &options).unwrap();
    assert_eq!(tv.banner, "device::FAKE::0");

    assert_eq!(
        tv.capabilities().unwrap(),
        "cpu_arch:armv7\nsdk_toolpath:/tmp\n"
    );

    let mut out = Vec::new();
    tv.shell("0 vd_applist", &mut out).unwrap();
    assert_eq!(out, "ran 0 vd_applist\n".repeat(200).into_bytes());

    assert!(tv.shell("refuse", &mut Vec::new()).unwrap().is_empty());

    let dir = std::env::temp_dir().join(format!("tv-sdb-test-{}", std::process::id()));
    std::fs::create_dir_all(&dir).unwrap();
    let big: Vec<u8> = (0..300_000u32).map(|i| (i * 7 % 251) as u8).collect();
    std::fs::write(dir.join("big.bin"), &big).unwrap();
    std::fs::write(dir.join("empty"), b"").unwrap();

    assert_eq!(
        tv.push(&dir.join("big.bin"), "/dir").unwrap(),
        "/dir/big.bin"
    );
    assert_eq!(
        tv.pull("/dir/big.bin", &dir.join("back.bin")).unwrap(),
        big.len() as u64
    );
    assert_eq!(std::fs::read(dir.join("back.bin")).unwrap(), big);

    tv.push(&dir.join("empty"), "/e").unwrap();
    assert_eq!(tv.pull("/e", &dir.join("e2")).unwrap(), 0);

    let err = tv.pull("/nope", &dir.join("nope")).unwrap_err();
    assert!(err.to_string().contains("cannot pull"), "{err}");
    assert!(!dir.join("nope").exists() && !dir.join(".nope.tv-sdb-part").exists());

    std::fs::write(dir.join("my app.tpk"), b"PK").unwrap();
    tv.install(&dir.join("my app.tpk"), &mut Vec::new())
        .unwrap();
    assert!(tv.install(Path::new("x.zip"), &mut Vec::new()).is_err());

    // A second connection right after the first.
    drop(tv);
    let mut again = Device::connect(&target, &options).unwrap();
    again.shell("0 getduid", &mut Vec::new()).unwrap();
    std::fs::remove_dir_all(&dir).unwrap();
}

#[test]
fn connect_fails_cleanly_when_tv_hangs_up() {
    let listener = TcpListener::bind("127.0.0.1:0").unwrap();
    let target = listener.local_addr().unwrap().to_string();
    std::thread::spawn(move || {
        for s in listener.incoming() {
            let mut s = s.unwrap();
            let _ = s.read(&mut [0; 24]);
        }
    });
    let options = Options {
        timeout: Duration::from_secs(2),
        verbose: false,
    };
    let err = Device::connect(&target, &options).err().unwrap();
    assert!(err.to_string().contains("closed by TV"), "{err}");
}
