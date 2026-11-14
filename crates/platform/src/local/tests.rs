use std::io;
use std::time::Duration;

use tokio::time::timeout;
use unikvm_protocol::local::{Status, query, respond};

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
        drop(endpoint.connect().await?);
        let stream = listener.accept().await?;
        assert!(
            respond(stream, || panic!("abandoned client sent no request"))
                .await
                .is_err()
        );
        listener.finish()?;

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
                    let stream = listener.accept().await?;
                    respond(stream, || expected).await?;
                    listener.finish()
                }
            );
            assert_eq!(received?, expected);
            served?;
        }
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
