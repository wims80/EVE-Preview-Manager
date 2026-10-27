fn main() {
    // ESI config read by release_config() in src/daemon/esi/worker.rs.
    for key in [
        "EPM_ESI_CLIENT_ID",
        "EPM_ESI_CALLBACK",
        "EPM_MAINTAINER_CONTACT",
        "EPM_ESI_COMPATIBILITY_DATE",
    ] {
        println!("cargo:rerun-if-env-changed={key}");
    }
}
