//! Hermetic runtime-image validation helper; never copied into the release image.
#[path = "../src/config.rs"]
#[allow(dead_code)]
mod config;

use std::{env, error::Error, fs, net::ToSocketAddrs};

fn error_chain(error: &reqwest::Error) -> String {
    let mut messages = error.to_string();
    let mut source = error.source();
    while let Some(cause) = source {
        messages.push_str(&format!(": {cause}"));
        source = cause.source();
    }
    messages.to_lowercase()
}

#[tokio::main]
async fn main() -> Result<(), Box<dyn Error>> {
    let certificate = reqwest::Certificate::from_pem(&fs::read("/fixture/ca.pem")?)?;
    for name in ["hosts-only.runtime.test", "dns-only.runtime.test"] {
        assert!(
            (name, 8443)
                .to_socket_addrs()?
                .any(|address| address.ip().is_loopback())
        );
    }
    let client = config::upstream_client_builder()
        .no_proxy()
        .add_root_certificate(certificate)
        .build()?;
    for name in ["hosts-only.runtime.test", "dns-only.runtime.test"] {
        let response = client
            .get(format!("https://{name}:8443/"))
            .send()
            .await?
            .error_for_status()?;
        assert_eq!(response.text().await?, "runtime TLS fixture");
    }
    let mismatch = client
        .get("https://wrong-name.runtime.test:8443/")
        .send()
        .await
        .expect_err("TLS must reject a certificate hostname mismatch");
    let message = error_chain(&mismatch);
    assert!(
        message.contains("certificate") && message.contains("name"),
        "{message}"
    );
    let untrusted = config::upstream_client_builder()
        .no_proxy()
        .build()?
        .get("https://dns-only.runtime.test:8443/")
        .send()
        .await
        .expect_err("TLS must reject an untrusted fixture CA");
    let message = error_chain(&untrusted);
    assert!(
        message.contains("unknownissuer") || message.contains("unknown issuer"),
        "{message}"
    );
    if env::args().any(|argument| argument == "--check-maps") {
        let maps = fs::read_to_string("/proc/self/maps")?;
        for line in maps.lines().filter(|line| line.contains(".so")) {
            println!("LOADED {}", line.split_whitespace().last().unwrap());
        }
    }
    println!("Host-file lookup, DNS A/AAAA, verified Rustls TLS and certificate rejection passed.");
    Ok(())
}
