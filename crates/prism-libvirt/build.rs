//! Generates the `REMOTE_PROC_*` procedure-number constants from the vendored
//! `remote_protocol.x`, so the list can never drift from the protocol.

use std::path::PathBuf;

use proc_macro2::{Ident, Span};
use quote::quote;

fn main() -> Result<(), Box<dyn std::error::Error>> {
    println!("cargo::rerun-if-changed=proto/remote_protocol.x");
    let src = std::fs::read_to_string("proto/remote_protocol.x")?;
    let body = src
        .split("enum remote_procedure {")
        .nth(1)
        .and_then(|s| s.split("};").next())
        .ok_or("remote_procedure enum not found")?;
    let mut consts = Vec::new();
    for line in body.lines() {
        let line = line.trim();
        let Some(rest) = line.strip_prefix("REMOTE_PROC_") else {
            continue;
        };
        let Some((name, num)) = rest.split_once('=') else {
            continue;
        };
        let num: u32 = num.trim().trim_end_matches(',').trim().parse()?;
        let ident = Ident::new(name.trim(), Span::call_site());
        let doc = format!("`REMOTE_PROC_{}`, procedure number {num}.", name.trim());
        consts.push(quote! {
            #[doc = #doc]
            pub const #ident: u32 = #num;
        });
    }
    if consts.is_empty() {
        return Err("no procedures parsed".into());
    }
    let file: syn::File = syn::parse2(quote! {
        #(#consts)*
    })?;
    let out = PathBuf::from(std::env::var("OUT_DIR")?).join("procs.rs");
    std::fs::write(out, prettyplease::unparse(&file))?;
    Ok(())
}
