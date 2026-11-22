#![cfg(unix)]

use std::{
    net::TcpListener,
    process::{Child, Command, Stdio},
    thread,
    time::{Duration, Instant},
};

struct ClientProcess(Child);

impl Drop for ClientProcess {
    fn drop(&mut self) {
        if matches!(self.0.try_wait(), Ok(None)) {
            let _ = self.0.kill();
            let _ = self.0.wait();
        }
    }
}

#[test]
fn sigint_and_sigterm_stop_client_during_handshake() {
    for signal in [libc::SIGINT, libc::SIGTERM] {
        let listener = TcpListener::bind(("127.0.0.1", 0)).unwrap();
        listener.set_nonblocking(true).unwrap();
        let url = format!("tcp://{}", listener.local_addr().unwrap());
        let mut client = ClientProcess(
            Command::new(env!("CARGO_BIN_EXE_snapclient-rs"))
                .args(["--logsink", "null", &url])
                .env_remove("RUST_LOG")
                .stdout(Stdio::null())
                .spawn()
                .unwrap(),
        );

        // Accept the connection but never complete the application handshake.
        let startup_deadline = Instant::now() + Duration::from_secs(5);
        let (_connection, _) = loop {
            match listener.accept() {
                Ok(connection) => break connection,
                Err(error) if error.kind() == std::io::ErrorKind::WouldBlock => {
                    assert!(
                        client.0.try_wait().unwrap().is_none(),
                        "client exited early"
                    );
                    assert!(Instant::now() < startup_deadline, "client did not connect");
                    thread::sleep(Duration::from_millis(10));
                }
                Err(error) => panic!("accept failed: {error}"),
            }
        };

        assert_eq!(
            unsafe { libc::kill(client.0.id() as libc::pid_t, signal) },
            0
        );
        let shutdown_deadline = Instant::now() + Duration::from_secs(3);
        loop {
            if let Some(status) = client.0.try_wait().unwrap() {
                assert!(status.success(), "signal {signal} resulted in {status}");
                break;
            }
            assert!(Instant::now() < shutdown_deadline, "shutdown timed out");
            thread::sleep(Duration::from_millis(10));
        }
    }
}
