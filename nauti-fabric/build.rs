// The RoCE data path's verbs shim (feature `rdma`): the inline verbs calls as plain C functions, linked with
// libibverbs.  The default build has no native dependency.
fn main() {
    println!("cargo:rerun-if-changed=csrc/rdma_shim.c");
    if std::env::var_os("CARGO_FEATURE_RDMA").is_some() {
        cc::Build::new().file("csrc/rdma_shim.c").warnings(true).compile("nauti_rdma_shim");
        println!("cargo:rustc-link-lib=ibverbs");
    }
}
