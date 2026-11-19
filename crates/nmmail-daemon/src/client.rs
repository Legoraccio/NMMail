//! Tiny IPC client used by `--activate` / `--show` (notification clicks).

use std::process::ExitCode;
use std::time::Duration;

use tokio::io::{AsyncBufReadExt, AsyncWriteExt, BufReader};

use crate::platform;

async fn call(method: &str, params: serde_json::Value) -> anyhow::Result<()> {
    let mut stream = None;
    for attempt in 0..50 {
        match platform::connect_ipc().await {
            Ok(s) => {
                stream = Some(s);
                break;
            }
            Err(_) => {
                if attempt == 0 {
                    platform::spawn_daemon()?;
                }
                tokio::time::sleep(Duration::from_millis(100)).await;
            }
        }
    }
    let stream = stream.ok_or_else(|| anyhow::anyhow!("daemon not reachable"))?;
    let (rd, mut wr) = tokio::io::split(stream);
    let req = serde_json::json!({ "id": 1, "method": "hello", "params": { "client": "activator" } });
    wr.write_all(format!("{req}\n").as_bytes()).await?;
    let req = serde_json::json!({ "id": 2, "method": method, "params": params });
    wr.write_all(format!("{req}\n").as_bytes()).await?;
    wr.flush().await?;
    let mut lines = BufReader::new(rd).lines();
    let wait = async {
        while let Some(line) = lines.next_line().await? {
            if line.contains("\"id\":2") {
                break;
            }
        }
        anyhow::Ok(())
    };
    let _ = tokio::time::timeout(Duration::from_secs(10), wait).await;
    Ok(())
}

fn block_on(fut: impl Future<Output = anyhow::Result<()>>) -> ExitCode {
    // Let the daemon / UI take the foreground (we got the user's click).
    platform::allow_foreground();
    let rt = match tokio::runtime::Builder::new_current_thread().enable_all().build() {
        Ok(rt) => rt,
        Err(_) => return ExitCode::FAILURE,
    };
    match rt.block_on(fut) {
        Ok(()) => ExitCode::SUCCESS,
        Err(_) => ExitCode::FAILURE,
    }
}

pub fn activate(uri: &str) -> ExitCode {
    let uri = uri.to_string();
    block_on(async move { call("activate", serde_json::json!({ "uri": uri })).await })
}

pub fn show() -> ExitCode {
    block_on(async { call("ui.show", serde_json::json!({})).await })
}
