use std::{
    io::{BufRead, BufReader, Read, Write},
    net::TcpStream,
    process::{Child, Command, Stdio},
    time::Duration,
};

fn command() -> Command {
    let command = Command::new(env!("CARGO_BIN_EXE_arrow-mc"));
    #[cfg(windows)]
    {
        use std::os::windows::process::CommandExt;
        let mut command = command;
        command.creation_flags(0x0800_0000); // CREATE_NO_WINDOW for the test helper.
        command
    }
    #[cfg(not(windows))]
    command
}

struct Process(Child);

impl Drop for Process {
    fn drop(&mut self) {
        let _ = self.0.kill();
        let _ = self.0.wait();
    }
}

#[test]
fn cli_help_and_invalid_limits_are_explicit() {
    let output = command().arg("--help").output().unwrap();
    assert!(output.status.success());
    let help = String::from_utf8(output.stdout).unwrap();
    assert!(help.contains("--bind IP"));
    assert!(help.contains("Online authentication is the default"));
    assert!(help.contains("gameplay is unavailable"));
    for args in [
        vec!["--io-workers", "0"],
        vec!["--max-connections", "0"],
        vec!["--port", "65536"],
        vec!["--unknown", "value"],
        vec!["--online-mode", "false"],
        vec!["--configuration-snapshot", "missing"],
    ] {
        let output = command().args(args).output().unwrap();
        assert!(!output.status.success());
        assert!(
            String::from_utf8(output.stderr)
                .unwrap()
                .contains("Arrow MC:")
        );
    }
}

#[test]
fn executable_really_binds_and_answers_a_java_status_packet() {
    let mut child = Process(
        command()
            .args([
                "--bind",
                "127.0.0.1",
                "--port",
                "0",
                "--description",
                "CLI smoke test",
                "--io-workers",
                "1",
            ])
            .stdout(Stdio::piped())
            .stderr(Stdio::piped())
            .spawn()
            .unwrap(),
    );
    let stdout = child.0.stdout.take().unwrap();
    let (send, receive) = std::sync::mpsc::channel();
    let reader = std::thread::spawn(move || {
        let mut line = String::new();
        let result = BufReader::new(stdout).read_line(&mut line).map(|_| line);
        let _ = send.send(result);
    });
    let line = receive
        .recv_timeout(Duration::from_secs(5))
        .unwrap()
        .unwrap();
    reader.join().unwrap();
    let address = line
        .strip_prefix("Arrow MC listening on ")
        .unwrap()
        .split_whitespace()
        .next()
        .unwrap();
    let mut client = TcpStream::connect(address).unwrap();
    client
        .set_read_timeout(Some(Duration::from_secs(2)))
        .unwrap();
    client
        .set_write_timeout(Some(Duration::from_secs(2)))
        .unwrap();
    // Literal fixture emitted by the actual 26.3-pre-2 handshake codec, then
    // the status request. This test does not use the implementation's encoder.
    client
        .write_all(&[
            0x13, 0x00, 0xce, 0x82, 0x80, 0x80, 0x04, 0x09, b'l', b'o', b'c', b'a', b'l', b'h',
            b'o', b's', b't', 0x63, 0xdd, 0x01, 0x01, 0x00,
        ])
        .unwrap();
    let mut length = 0usize;
    for shift in (0..21).step_by(7) {
        let mut byte = [0];
        client.read_exact(&mut byte).unwrap();
        length |= usize::from(byte[0] & 127) << shift;
        if byte[0] & 128 == 0 {
            break;
        }
    }
    assert!((1..1024).contains(&length));
    let mut body = vec![0; length];
    client.read_exact(&mut body).unwrap();
    assert_eq!(body[0], 0);
    let payload_start = body[1..].iter().position(|byte| byte & 128 == 0).unwrap() + 2;
    let json: serde_json::Value = serde_json::from_slice(&body[payload_start..]).unwrap();
    assert_eq!(json["description"], "CLI smoke test");
    assert_eq!(json["players"]["online"], 0);
    assert_eq!(json["version"]["name"], "26.3 Pre-Release 2");
    assert_eq!(json["version"]["protocol"], 1_073_742_158);
}
