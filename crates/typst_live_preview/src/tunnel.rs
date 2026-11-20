//! A local port forwarded to a remote Tinymist data plane.
//!
//! Tinymist serves the preview data plane on the loopback interface of the
//! machine it runs on. For an SSH project that machine is not this one, so the
//! pane cannot reach `127.0.0.1:<remote port>` directly. The project connection
//! already owns an SSH control socket, so we ask it for an `ssh -N -L` command.
//! That client either keeps the forward itself or, when a ControlMaster is
//! connected, hands the listener to the master and exits.

use std::{
    future::Future,
    sync::{Mutex, PoisonError},
    time::{Duration, Instant},
};

use anyhow::{Context as _, anyhow};
use gpui::{AsyncApp, Entity};
use remote::{CommandTemplate, RemoteClient};
use util::command::{Child, Stdio};

/// How long `ssh -L` gets to start accepting connections.
const TUNNEL_TIMEOUT: Duration = Duration::from_secs(10);
const TUNNEL_POLL_INTERVAL: Duration = Duration::from_millis(50);

/// A forwarded port together with the process that created it.
pub(crate) struct PortForward {
    child: Mutex<Option<Child>>,
}

impl PortForward {
    /// Forwards `remote_port` on the project host to a fresh local port.
    ///
    /// Returns the local port and the running tunnel. Waiting for the local
    /// port to accept connections here means the preview session's first
    /// connect attempt succeeds instead of racing the SSH handshake.
    pub(crate) async fn open_in_app(
        remote_client: &Entity<RemoteClient>,
        remote_port: u16,
        cx: &mut AsyncApp,
    ) -> anyhow::Result<(u16, Self)> {
        let local_port = reserve_local_port()?;
        let forwards = vec![(local_port, "127.0.0.1".to_owned(), remote_port)];
        let template =
            cx.update(|cx| remote_client.read(cx).build_forward_ports_command(forwards))?;

        let forward = Self::start(&template, local_port, || {
            cx.background_executor().timer(TUNNEL_POLL_INTERVAL)
        })
        .await
        .with_context(|| {
            format!("could not forward remote port {remote_port} to local port {local_port}")
        })?;

        log::info!(
            "typst live preview: forwarding local port {local_port} to remote port {remote_port}"
        );
        Ok((local_port, forward))
    }

    /// Spawns the forwarding command and waits until the local port accepts
    /// connections.
    ///
    /// `sleep` is injected so tests can drive the retry loop without a GPUI
    /// context.
    async fn start<F, Fut>(
        template: &CommandTemplate,
        local_port: u16,
        mut sleep: F,
    ) -> anyhow::Result<Self>
    where
        F: FnMut() -> Fut,
        Fut: Future<Output = ()>,
    {
        let mut command = util::command::new_command(&template.program);
        command
            .args(&template.args)
            .envs(&template.env)
            .stdin(Stdio::null())
            .stdout(Stdio::null())
            .stderr(Stdio::null())
            .kill_on_drop(true);
        let child = command
            .spawn()
            .with_context(|| format!("failed to start `{}`", template.program))?;

        let forward = Self {
            child: Mutex::new(Some(child)),
        };
        forward.wait_until_ready(local_port, &mut sleep).await?;
        Ok(forward)
    }

    async fn wait_until_ready<F, Fut>(&self, local_port: u16, sleep: &mut F) -> anyhow::Result<()>
    where
        F: FnMut() -> Fut,
        Fut: Future<Output = ()>,
    {
        let deadline = Instant::now() + TUNNEL_TIMEOUT;
        loop {
            match smol::net::TcpStream::connect(("127.0.0.1", local_port)).await {
                Ok(_) => return Ok(()),
                Err(_) => {
                    if let Some(status) = self.try_status()? {
                        if !status.success() {
                            return Err(anyhow!("the SSH command exited with {status}"));
                        }

                        // When an SSH ControlMaster is already connected,
                        // `ssh -N -L` hands the listener to the master and
                        // exits successfully; the master keeps the forward
                        // alive. Keep polling for the listener it owns instead
                        // of treating this exit as a failure.
                    }

                    if Instant::now() >= deadline {
                        return Err(anyhow!("timed out after {TUNNEL_TIMEOUT:?}"));
                    }
                    sleep().await;
                }
            }
        }
    }

    fn try_status(&self) -> std::io::Result<Option<std::process::ExitStatus>> {
        let mut child = self.child.lock().unwrap_or_else(PoisonError::into_inner);
        match child.as_mut() {
            Some(child) => child.try_status(),
            None => Ok(None),
        }
    }

    /// Stops the SSH process. Safe to call more than once.
    pub(crate) fn stop(&self) {
        let mut child = self.child.lock().unwrap_or_else(PoisonError::into_inner);
        if let Some(mut child) = child.take() {
            let _ = child.kill();
        }
    }
}

impl Drop for PortForward {
    fn drop(&mut self) {
        self.stop();
    }
}

/// Picks a port that is free right now. The tunnel binds it moments later;
/// nothing else in the editor allocates from this range in between.
fn reserve_local_port() -> anyhow::Result<u16> {
    let listener = std::net::TcpListener::bind(("127.0.0.1", 0))
        .context("failed to reserve a local port for the preview tunnel")?;
    let port = listener.local_addr()?.port();
    drop(listener);
    Ok(port)
}

#[cfg(test)]
mod tests {
    use std::{sync::Mutex, time::Duration};

    use remote::CommandTemplate;

    use super::{PortForward, reserve_local_port};

    #[cfg(unix)]
    fn exit_template(code: u8) -> CommandTemplate {
        CommandTemplate {
            program: "/bin/sh".into(),
            args: vec!["-c".into(), format!("exit {code}")],
            env: Default::default(),
        }
    }

    /// Reproduces the SSH ControlMaster case: the forwarding client exits with
    /// status 0 while the master binds the local port shortly afterwards.
    #[cfg(unix)]
    #[test]
    fn a_master_owned_forward_is_ready_after_the_client_exits() {
        let port = reserve_local_port().expect("a port should be available");
        let listener_thread = std::thread::spawn(move || {
            std::thread::sleep(Duration::from_millis(100));
            let listener = std::net::TcpListener::bind(("127.0.0.1", port))
                .expect("the master should be able to bind its forwarded port");
            std::thread::sleep(Duration::from_millis(500));
            drop(listener);
        });

        let forward = smol::block_on(PortForward::start(&exit_template(0), port, || async {
            std::thread::sleep(Duration::from_millis(10));
        }))
        .expect("a successful client exit must not abort a forward owned by the master");

        forward.stop();
        listener_thread
            .join()
            .expect("listener thread should finish");
    }

    #[cfg(unix)]
    #[test]
    fn a_failed_forwarding_client_is_reported_immediately() {
        let port = reserve_local_port().expect("a port should be available");

        let error = match smol::block_on(PortForward::start(&exit_template(7), port, || async {
            std::thread::sleep(Duration::from_millis(10));
        })) {
            Ok(_) => panic!("a non-zero SSH exit must fail the forward"),
            Err(error) => error,
        };

        assert!(
            error.to_string().contains("exited with"),
            "unexpected error: {error}"
        );
    }

    /// End-to-end harness for the real SSH path. It starts a ControlMaster,
    /// forwards a local port to the host's SSH server, and checks that the
    /// tunnel code accepts the forward after the mux client exits.
    ///
    /// ```sh
    /// SUZURI_TEST_SSH_HOST=venus-linux cargo test -p typst_live_preview \
    ///   forwards_through_a_real_ssh_control_master -- --ignored --nocapture
    /// ```
    #[cfg(unix)]
    #[test]
    #[ignore = "requires SUZURI_TEST_SSH_HOST and network access"]
    fn forwards_through_a_real_ssh_control_master() {
        use std::{io::Read as _, net::TcpStream, thread};
        use util::command::{Stdio, new_command};

        let host = std::env::var("SUZURI_TEST_SSH_HOST")
            .expect("set SUZURI_TEST_SSH_HOST to an SSH host alias");
        let remote_port = std::env::var("SUZURI_TEST_SSH_REMOTE_PORT")
            .ok()
            .and_then(|port| port.parse::<u16>().ok())
            .unwrap_or(22);
        let socket_path =
            std::env::temp_dir().join(format!("suzuri-tunnel-test-{}.sock", std::process::id()));
        let _ = std::fs::remove_file(&socket_path);

        let mut master_command = new_command("ssh");
        let mut master = master_command
            .args([
                "-N",
                "-o",
                "ControlPersist=no",
                "-o",
                "ControlMaster=yes",
                "-o",
            ])
            .arg(format!("ControlPath={}", socket_path.display()))
            .arg(&host)
            .stdin(Stdio::null())
            .stdout(Stdio::null())
            .stderr(Stdio::null())
            .spawn()
            .expect("failed to start the SSH ControlMaster");

        for _ in 0..200 {
            if socket_path.exists() {
                break;
            }
            thread::sleep(Duration::from_millis(50));
        }
        assert!(
            socket_path.exists(),
            "the SSH ControlMaster did not create its socket"
        );

        let local_port = reserve_local_port().expect("a port should be available");
        let template = CommandTemplate {
            program: "ssh".into(),
            args: vec![
                "-o".into(),
                "ControlMaster=no".into(),
                "-o".into(),
                format!("ControlPath={}", socket_path.display()),
                "-N".into(),
                "-L".into(),
                format!("{local_port}:127.0.0.1:{remote_port}"),
                host,
            ],
            env: Default::default(),
        };

        let forward = smol::block_on(PortForward::start(&template, local_port, || async {
            std::thread::sleep(Duration::from_millis(50));
        }))
        .expect("the forward should be ready even though the mux client exits");

        let mut stream = TcpStream::connect(("127.0.0.1", local_port))
            .expect("the forwarded local port should accept connections");
        stream
            .set_read_timeout(Some(Duration::from_secs(2)))
            .expect("read timeout should be supported");
        let mut banner = [0; 4];
        stream
            .read_exact(&mut banner)
            .expect("the forward should reach the remote SSH server");
        assert_eq!(&banner, b"SSH-");

        forward.stop();
        master.kill().ok();
        smol::block_on(master.status()).ok();
        let _ = std::fs::remove_file(&socket_path);
    }

    #[test]
    fn reserved_ports_are_free() {
        let port = reserve_local_port().expect("a port should be available");
        assert!(std::net::TcpListener::bind(("127.0.0.1", port)).is_ok());
    }

    #[cfg(unix)]
    #[test]
    fn stopping_the_tunnel_kills_its_process() {
        let child = util::command::new_command("sleep")
            .arg("60")
            .spawn()
            .expect("sleep should start");
        let forward = PortForward {
            child: Mutex::new(Some(child)),
        };

        forward.stop();
        assert!(
            forward.child.lock().unwrap().is_none(),
            "stop should take and kill the SSH child"
        );

        // Closing an already-stopped preview must not panic.
        forward.stop();
    }
}
