//! Optional experiment-only GUI diagnostics. No filesystem work on the UI.
use crossbeam_channel::{bounded, Sender};
use serde_json::{json, Value};
use std::{io::Write, sync::OnceLock, time::Instant};

static TRACE: OnceLock<(Instant, Sender<Value>)> = OnceLock::new();

pub fn init() {
    let Some(path) = std::env::var_os("SCRUTIVUE_NEF_UI_TRACE") else {
        return;
    };
    let (tx, rx) = bounded::<Value>(256);
    let _ = TRACE.set((Instant::now(), tx));
    std::thread::Builder::new()
        .name("nef-ui-trace".into())
        .spawn(move || {
            let result = (|| -> anyhow::Result<()> {
                let mut file = std::fs::OpenOptions::new()
                    .write(true)
                    .create_new(true)
                    .open(path)?;
                for record in rx {
                    serde_json::to_writer(&mut file, &record)?;
                    file.write_all(b"\n")?;
                }
                Ok(())
            })();
            if let Err(error) = result {
                eprintln!("NEF UI trace failed: {error:#}");
            }
        })
        .expect("NEF UI trace writer");
    record(
        "trace_started",
        json!({"scope":"GUI acceptance timestamps; not GPU presentation or screen visibility"}),
    );
}

pub fn record(event: &str, data: Value) {
    if let Some((started, tx)) = TRACE.get() {
        let _ = tx.try_send(
            json!({"ms":started.elapsed().as_secs_f64()*1000.,"event":event,"data":data}),
        );
    }
}
