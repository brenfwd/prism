//! Connects to a libvirt socket, then prints the library version, host name,
//! host CPU and memory, and every domain with its UUID and state.
//!
//! Usage: `cargo run -p prism-libvirt --example probe [socket] [uri]`. The
//! socket defaults to `/run/libvirt/virtqemud-sock` and the URI to
//! `qemu:///system`.

#[tokio::main(flavor = "current_thread")]
async fn main() -> Result<(), prism_libvirt::Error> {
    let mut args = std::env::args().skip(1);
    let sock = args
        .next()
        .unwrap_or_else(|| "/run/libvirt/virtqemud-sock".into());
    let uri = args.next().unwrap_or_else(|| "qemu:///system".into());
    let c = prism_libvirt::Client::connect(&sock, &uri).await?;
    println!("libvirt {}", c.lib_version().await?);
    println!("hostname {}", c.hostname().await?);
    println!("{:?}", c.node_info().await?);
    println!("free {}", c.free_memory().await?);
    for d in c.list_all_domains(0).await? {
        let info = c.get_info(&d).await?;
        println!(
            "{} {} {:?}",
            d.name,
            prism_libvirt::format_uuid(&d.uuid),
            info
        );
    }
    Ok(())
}
