use std::io;
use std::time::Duration;

use tokio::time::timeout;
use unikvm_protocol::local::{
    ClipboardPhase, ClipboardStatus, Status, query, query_clipboard, respond,
    respond_with_clipboard,
};

use super::Endpoint;

#[tokio::test]
async fn native_status_transport_retains_endpoint_and_recovers_after_abandoned_client()
-> io::Result<()> {
    let namespace = format!("unikvm-test-{}", std::process::id());
    #[cfg(unix)]
    let _directory = TestDirectory(namespace.clone());
    let make_endpoint = || Endpoint::for_test(&namespace);
    let mut listener = make_endpoint()?.listen()?;
    assert!(make_endpoint()?.listen().is_err());

    timeout(Duration::from_secs(5), async {
        let endpoint = make_endpoint()?;
        let (client, stream) = tokio::join!(endpoint.connect(), listener.accept());
        let stream = stream?;
        drop(client?);
        assert!(
            respond(stream, || panic!("abandoned client sent no request"))
                .await
                .is_err()
        );
        listener.finish()?;

        // Unix queues connections even if the client closes before accept. Peer
        // credential lookup can then fail on macOS; the next live client must work.
        #[cfg(unix)]
        drop(endpoint.connect().await?);

        let expected = Status {
            pid: std::process::id(),
            uptime_secs: 5,
        };
        for _ in 0..3 {
            let (received, served) = tokio::join!(
                async {
                    let mut client = endpoint.connect().await?;
                    query(&mut client).await
                },
                async {
                    loop {
                        let stream = listener.accept().await?;
                        match respond(stream, || expected).await {
                            Ok(()) => return listener.finish(),
                            // Linux can authenticate the queued abandoned client;
                            // macOS discards it during peer credential lookup.
                            Err(error) if error.kind() == io::ErrorKind::UnexpectedEof => {
                                listener.finish()?;
                            }
                            Err(error) => return Err(error),
                        }
                    }
                }
            );
            assert_eq!(received?, expected);
            served?;
        }
        let clipboard = ClipboardStatus {
            connected: true,
            outgoing: ClipboardPhase::Ready,
            incoming: ClipboardPhase::Copying,
        };
        let (received, served) = tokio::join!(
            async {
                let mut client = endpoint.connect().await?;
                query_clipboard(&mut client).await
            },
            async {
                let stream = listener.accept().await?;
                respond_with_clipboard(stream, || expected, || clipboard).await?;
                listener.finish()
            }
        );
        assert_eq!(received?, clipboard);
        served?;
        Ok::<_, io::Error>(())
    })
    .await??;

    drop(listener);
    // On Windows, the event loop may still be closing cancelled pipe I/O at this point.
    #[cfg(unix)]
    let _restarted = make_endpoint()?.listen()?;
    Ok(())
}

#[cfg(unix)]
struct TestDirectory(String);

#[cfg(unix)]
impl Drop for TestDirectory {
    fn drop(&mut self) {
        let path = std::path::Path::new("/tmp").join(&self.0);
        let _ = std::fs::remove_file(path.join("agent.lock"));
        let _ = std::fs::remove_dir(path);
    }
}
